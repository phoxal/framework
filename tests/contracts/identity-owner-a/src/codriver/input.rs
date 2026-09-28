//! The private input module of this codriver binary: same module and type
//! name as the driver binary's, different schema.

/// One private reading, carried as text.
#[phoxal::message]
pub struct Reading {
    #[phoxal(tag = 1)]
    pub value: String,
}

#[cfg(test)]
mod tests {
    use super::Reading;
    use phoxal::schema::MessageSchema;

    #[test]
    fn a_second_binary_of_one_package_derives_a_distinct_identity() {
        assert_eq!(
            Reading::WIRE_NAME,
            "phoxal.private.identity_2downer_2da.codriver.input.Reading"
        );
        assert_ne!(
            Reading::WIRE_NAME,
            "phoxal.private.identity_2downer_2da.driver.input.Reading",
            "two binaries of one package never share an identity"
        );
        assert!(Reading::retain_schema() > 0);
    }
}
