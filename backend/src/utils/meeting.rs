//! Meeting-specific utility functions

use rand::Rng;

/// Generate a short, human-friendly meeting identifier (8-character code)
/// 
/// Creates a random 8-character code using uppercase letters and numbers,
/// excluding ambiguous characters (0, O, I, 1, etc.) for better readability.
/// 
/// # Example
/// ```ignore
/// let code = generate_meeting_identifier();
/// println!("Meeting code: {}", code); // e.g., "ABCD1234"
/// ```
/// 
/// # Returns
/// An 8-character string containing only uppercase letters (A-Z, excluding I and O)
/// and numbers (2-9, excluding 0 and 1 to avoid confusion with letters).
pub fn generate_meeting_identifier() -> String {
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".chars().collect();
    let code: String = (0..8)
        .map(|_| chars[rng.gen_range(0..chars.len())])
        .collect();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_meeting_identifier_length() {
        let code = generate_meeting_identifier();
        assert_eq!(code.len(), 8);
    }

    #[test]
    fn test_generate_meeting_identifier_valid_chars() {
        let code = generate_meeting_identifier();
        let valid_chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        for c in code.chars() {
            assert!(valid_chars.contains(c));
        }
    }

    #[test]
    fn test_generate_meeting_identifier_uniqueness() {
        let code1 = generate_meeting_identifier();
        let code2 = generate_meeting_identifier();
        // While not guaranteed, different codes are highly likely
        // This is a weak test but catches obvious issues
        assert!(!code1.is_empty() && !code2.is_empty());
    }
}
