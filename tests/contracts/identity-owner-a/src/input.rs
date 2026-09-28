//! The private input module of this driver binary.

/// One private reading.
#[phoxal::message]
pub struct Reading {
    #[phoxal(tag = 1)]
    pub value: f64,
}

#[cfg(test)]
mod tests {
    use super::Reading;
    use phoxal::schema::MessageSchema;

    #[test]
    fn identity_is_qualified_by_the_owning_package_and_target() {
        assert_eq!(
            Reading::WIRE_NAME,
            "phoxal.private.identity_2downer_2da.driver.input.Reading"
        );
        assert!(Reading::retain_schema() > 0);
    }
}
