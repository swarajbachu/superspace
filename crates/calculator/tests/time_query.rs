//! Regression coverage for natural-language time-zone queries.

use superspace_calculator::TimeQuery;

#[test]
fn parses_spaced_period_with_explicit_destination_zone() {
    let query = TimeQuery::parse("1 pm pst in gmt", "Asia/Kolkata")
        .expect("spaced time query with destination");
    let conversion = query.convert().expect("time conversion");

    assert_eq!(conversion.input_time, "1:00 PM");
    assert_eq!(conversion.from_zone, "PDT");
    assert_eq!(conversion.output_time, "8:00 PM");
    assert_eq!(conversion.to_zone, "UTC");
}
