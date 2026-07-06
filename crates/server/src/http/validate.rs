//! A tiny, dependency-free request-DTO validator.
//!
//! Every write endpoint runs its decoded DTO through a [`Validator`] before it
//! touches a repository. Accumulating *all* field errors (rather than failing on
//! the first) means a malformed payload comes back as one 400 listing every
//! problem, which is friendlier for the SPA client than a fix-one-see-the-next
//! round-trip. The collected failures become [`ApiError::Validation`], which the
//! envelope renders as the structured `details` array.
//!
//! This is input *shape* validation (non-empty ids, positive quantities, a
//! 3-letter currency) — deliberately not business policy. The domain invariants
//! (copy caps, ledger solvency, one-live-ticket) stay where they belong: the
//! aggregates and the database `CHECK`s the repository adapters surface.

use super::envelope::{ApiError, FieldError};

/// Accumulates field-level validation failures for one request DTO.
#[derive(Default)]
pub struct Validator {
    errors: Vec<FieldError>,
}

impl Validator {
    /// Start with no errors recorded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure for `field` with the given `message`.
    pub fn error(&mut self, field: &str, message: &str) {
        self.errors.push(FieldError {
            field: field.to_string(),
            message: message.to_string(),
        });
    }

    /// Require `condition` to hold, recording `message` against `field` if not.
    pub fn require(&mut self, condition: bool, field: &str, message: &str) {
        if !condition {
            self.error(field, message);
        }
    }

    /// Require `value` to contain a non-whitespace character.
    pub fn non_empty(&mut self, field: &str, value: &str) {
        self.require(!value.trim().is_empty(), field, "must not be empty");
    }

    /// Finish: `Ok(())` if clean, otherwise an [`ApiError::Validation`] carrying
    /// every accumulated field error.
    pub fn finish(self) -> Result<(), ApiError> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Validation(self.errors))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_validator_finishes_ok() {
        let mut v = Validator::new();
        v.non_empty("id", "abc");
        v.require(1 > 0, "n", "must be positive");
        assert!(v.finish().is_ok());
    }

    #[test]
    fn failures_accumulate_into_one_validation_error() {
        let mut v = Validator::new();
        v.non_empty("id", "   "); // whitespace-only is empty
        v.require(false, "quantity", "must be greater than zero");
        match v.finish() {
            Err(ApiError::Validation(fields)) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].field, "id");
                assert_eq!(fields[1].field, "quantity");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }
}
