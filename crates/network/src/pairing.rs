use std::fmt::{self, Write as _};

use snow::{Builder, HandshakeState, TransportState, params::NoiseParams};
use thiserror::Error;

const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const MAX_MESSAGE_SIZE: usize = 65_535;
const PAIRING_CONTEXT: &[u8] = b"superspace pairing code v1";

/// Long-lived Noise static identity persisted in the OS credential store.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceKeypair {
    private: Vec<u8>,
    public: Vec<u8>,
}

impl DeviceKeypair {
    /// Generate a new X25519 static identity with the system random source.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when the Noise provider cannot generate a key.
    pub fn generate() -> Result<Self, PairingError> {
        let keypair = builder()?.generate_keypair()?;
        Ok(Self {
            private: keypair.private,
            public: keypair.public,
        })
    }

    /// Restore a keypair after verifying that both keys have the expected X25519 length.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::InvalidKey`] for malformed key material.
    pub fn from_bytes(private: Vec<u8>, public: Vec<u8>) -> Result<Self, PairingError> {
        if private.len() != 32 || public.len() != 32 {
            return Err(PairingError::InvalidKey);
        }
        Ok(Self { private, public })
    }

    /// Public identity exchanged and pinned after user verification.
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// Private bytes for persistence in Keychain or Secret Service.
    #[must_use]
    pub fn private_key(&self) -> &[u8] {
        &self.private
    }
}

impl fmt::Debug for DeviceKeypair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceKeypair")
            .field("private", &"[REDACTED]")
            .field("public", &hex_preview(&self.public))
            .finish()
    }
}

/// Six-digit short authentication string shown on both devices.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PairingCode(u32);

impl PairingCode {
    /// Numeric value in the range `000000..=999999`.
    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PairingCode({self})")
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:06}", self.0)
    }
}

/// Pairing and encrypted-channel failures.
#[derive(Debug, Error)]
pub enum PairingError {
    /// Noise protocol setup or message processing failed.
    #[error("secure pairing failed")]
    Noise(#[from] snow::Error),
    /// Persisted identity has an invalid size.
    #[error("device identity is malformed")]
    InvalidKey,
    /// Handshake method was called in an invalid order.
    #[error("pairing messages arrived out of order")]
    InvalidState,
    /// Plaintext exceeds the Noise transport message limit.
    #[error("encrypted message is too large")]
    MessageTooLarge,
}

/// Initiating side of the three-message verified Noise XX handshake.
pub struct PairingInitiator {
    state: Option<HandshakeState>,
}

impl PairingInitiator {
    /// Start pairing with a stable local identity.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when Noise initialization fails.
    pub fn new(identity: &DeviceKeypair) -> Result<Self, PairingError> {
        let state = builder()?
            .local_private_key(identity.private_key())?
            .build_initiator()?;
        Ok(Self { state: Some(state) })
    }

    /// Produce the first handshake packet.
    ///
    /// # Errors
    ///
    /// Returns an error when called after the handshake has already finished.
    pub fn initial_message(&mut self) -> Result<Vec<u8>, PairingError> {
        let state = self.state.as_mut().ok_or(PairingError::InvalidState)?;
        write_handshake(state)
    }

    /// Consume the responder packet, produce the final packet, and enter transport mode.
    ///
    /// The caller must compare [`InitiatorFinish::code`] with the responder before trusting
    /// [`InitiatorFinish::remote_static_key`].
    ///
    /// # Errors
    ///
    /// Returns an error for malformed packets or invalid handshake order.
    pub fn finish(mut self, response: &[u8]) -> Result<InitiatorFinish, PairingError> {
        let mut state = self.state.take().ok_or(PairingError::InvalidState)?;
        read_handshake(&mut state, response)?;
        let code = pairing_code(state.get_handshake_hash());
        let remote_static_key = state
            .get_remote_static()
            .ok_or(PairingError::InvalidState)?
            .to_vec();
        let confirmation = write_handshake(&mut state)?;
        let channel = SecureChannel {
            state: state.into_transport_mode()?,
        };
        Ok(InitiatorFinish {
            confirmation,
            code,
            remote_static_key,
            channel,
        })
    }
}

/// Result returned to the initiating UI before the user confirms the code.
pub struct InitiatorFinish {
    /// Third and final handshake packet to send.
    pub confirmation: Vec<u8>,
    /// Code that must match the responder UI.
    pub code: PairingCode,
    /// Responder identity to persist only after matching confirmation.
    pub remote_static_key: Vec<u8>,
    /// Encrypted channel ready after confirmation is transmitted.
    pub channel: SecureChannel,
}

/// Responding side of the verified Noise XX handshake.
pub struct PairingResponder {
    state: Option<HandshakeState>,
    code: Option<PairingCode>,
}

impl PairingResponder {
    /// Prepare a responder with a stable local identity.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError`] when Noise initialization fails.
    pub fn new(identity: &DeviceKeypair) -> Result<Self, PairingError> {
        let state = builder()?
            .local_private_key(identity.private_key())?
            .build_responder()?;
        Ok(Self {
            state: Some(state),
            code: None,
        })
    }

    /// Consume the initial packet and return the response plus verification code.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed packets or invalid handshake order.
    pub fn respond(&mut self, initial: &[u8]) -> Result<ResponderReply, PairingError> {
        let state = self.state.as_mut().ok_or(PairingError::InvalidState)?;
        read_handshake(state, initial)?;
        let message = write_handshake(state)?;
        let code = pairing_code(state.get_handshake_hash());
        self.code = Some(code);
        Ok(ResponderReply { message, code })
    }

    /// Consume the final initiator packet and enter transport mode.
    ///
    /// The caller must verify `expected_code` against the value already displayed before saving the
    /// returned remote identity.
    ///
    /// # Errors
    ///
    /// Returns an error for code mismatch, malformed packets, or invalid handshake order.
    pub fn finish(
        mut self,
        confirmation: &[u8],
        expected_code: PairingCode,
    ) -> Result<(Vec<u8>, SecureChannel), PairingError> {
        if self.code != Some(expected_code) {
            return Err(PairingError::InvalidState);
        }
        let mut state = self.state.take().ok_or(PairingError::InvalidState)?;
        read_handshake(&mut state, confirmation)?;
        let remote_static_key = state
            .get_remote_static()
            .ok_or(PairingError::InvalidState)?
            .to_vec();
        Ok((
            remote_static_key,
            SecureChannel {
                state: state.into_transport_mode()?,
            },
        ))
    }
}

/// Response packet and the code displayed by the receiving device.
pub struct ResponderReply {
    /// Second Noise XX handshake packet.
    pub message: Vec<u8>,
    /// Short authentication string.
    pub code: PairingCode,
}

/// Authenticated, forward-secret Noise transport.
pub struct SecureChannel {
    state: TransportState,
}

impl SecureChannel {
    /// Encrypt one application frame, advancing the send nonce.
    ///
    /// # Errors
    ///
    /// Returns [`PairingError::MessageTooLarge`] for oversized frames or a Noise transport error.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, PairingError> {
        if plaintext.len() > MAX_MESSAGE_SIZE - 16 {
            return Err(PairingError::MessageTooLarge);
        }
        let mut output = vec![0; plaintext.len() + 16];
        let length = self.state.write_message(plaintext, &mut output)?;
        output.truncate(length);
        Ok(output)
    }

    /// Decrypt and authenticate one application frame, advancing the receive nonce.
    ///
    /// # Errors
    ///
    /// Returns a Noise error when authentication fails or the frame is malformed.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, PairingError> {
        if ciphertext.len() > MAX_MESSAGE_SIZE {
            return Err(PairingError::MessageTooLarge);
        }
        let mut output = vec![0; ciphertext.len()];
        let length = self.state.read_message(ciphertext, &mut output)?;
        output.truncate(length);
        Ok(output)
    }
}

fn builder() -> Result<Builder<'static>, PairingError> {
    let parameters: NoiseParams = NOISE_PATTERN.parse()?;
    Ok(Builder::new(parameters))
}

fn write_handshake(state: &mut HandshakeState) -> Result<Vec<u8>, PairingError> {
    let mut output = vec![0; MAX_MESSAGE_SIZE];
    let length = state.write_message(&[], &mut output)?;
    output.truncate(length);
    Ok(output)
}

fn read_handshake(state: &mut HandshakeState, message: &[u8]) -> Result<(), PairingError> {
    if message.len() > MAX_MESSAGE_SIZE {
        return Err(PairingError::MessageTooLarge);
    }
    let mut payload = [0_u8; 1];
    state.read_message(message, &mut payload)?;
    Ok(())
}

fn pairing_code(handshake_hash: &[u8]) -> PairingCode {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PAIRING_CONTEXT);
    hasher.update(handshake_hash);
    let digest = hasher.finalize();
    let prefix = u32::from_be_bytes(digest.as_bytes()[..4].try_into().expect("four-byte prefix"));
    PairingCode(prefix % 1_000_000)
}

fn hex_preview(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(6)
        .fold(String::new(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xx_pairing_authenticates_identities_and_encrypts_both_directions() {
        let initiator_identity = DeviceKeypair::generate().expect("initiator identity");
        let responder_identity = DeviceKeypair::generate().expect("responder identity");
        let mut initiator = PairingInitiator::new(&initiator_identity).expect("initiator");
        let mut responder = PairingResponder::new(&responder_identity).expect("responder");

        let initial = initiator.initial_message().expect("initial message");
        let reply = responder.respond(&initial).expect("response");
        let mut initiated = initiator.finish(&reply.message).expect("initiator finish");
        assert_eq!(initiated.code, reply.code);
        assert_eq!(initiated.remote_static_key, responder_identity.public_key());

        let (remote_key, mut receiving_channel) = responder
            .finish(&initiated.confirmation, reply.code)
            .expect("responder finish");
        assert_eq!(remote_key, initiator_identity.public_key());

        let ciphertext = initiated.channel.encrypt(b"mac to linux").expect("encrypt");
        assert_ne!(ciphertext, b"mac to linux");
        assert_eq!(
            receiving_channel.decrypt(&ciphertext).expect("decrypt"),
            b"mac to linux"
        );

        let ciphertext = receiving_channel
            .encrypt(b"linux to mac")
            .expect("encrypt back");
        assert_eq!(
            initiated
                .channel
                .decrypt(&ciphertext)
                .expect("decrypt back"),
            b"linux to mac"
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let first = DeviceKeypair::generate().expect("first identity");
        let second = DeviceKeypair::generate().expect("second identity");
        let mut initiator = PairingInitiator::new(&first).expect("initiator");
        let mut responder = PairingResponder::new(&second).expect("responder");
        let initial = initiator.initial_message().expect("initial");
        let reply = responder.respond(&initial).expect("reply");
        let mut initiated = initiator.finish(&reply.message).expect("initiated");
        let (_, mut receiving_channel) = responder
            .finish(&initiated.confirmation, reply.code)
            .expect("responded");
        let mut ciphertext = initiated.channel.encrypt(b"secret").expect("encrypt");
        ciphertext[0] ^= 1;
        assert!(receiving_channel.decrypt(&ciphertext).is_err());
    }

    #[test]
    fn key_debug_output_never_contains_private_bytes() {
        let identity = DeviceKeypair::generate().expect("identity");
        let debug = format!("{identity:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains(&hex_preview(identity.private_key())));
    }
}
