//! Destination classification (Part H §66): a pre-toll hook that refuses premium-rate,
//! shared-cost, satellite and other high-risk number ranges BEFORE any provider action.
//!
//! Rules live in `telecom_destination_classes` as (country, national-significant prefix,
//! class). The executor classifies the resolved destination — which exists only inside
//! the execution boundary — by the LONGEST enabled prefix within the destination
//! country. A destination with no matching rule is *unclassified*: that is not
//! "standard", and `TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY` decides whether it may
//! proceed (`ALLOW`, the default while no rule data is loaded) or is refused (`DENY`,
//! once an operator has loaded the ranges of the countries routes serve).
//!
//! Neither the number nor the matched prefix is ever logged with the classification:
//! only the class (a bounded vocabulary) leaves this module as a metric label / code.

use sea_orm::{ConnectionTrait, FromQueryResult};

use crate::{db, error::TelephonyResult, providers::ExecutionNumber};

crate::domain::vocab!(
    /// Destination classes (`telecom_destination_classes.class`).
    DestinationClass {
        Standard = "STANDARD",
        Premium = "PREMIUM",
        SharedCost = "SHARED_COST",
        Satellite = "SATELLITE",
        HighRisk = "HIGH_RISK",
    }
);

impl DestinationClass {
    /// The default blocked set (`TELEPHONY_BLOCKED_DESTINATION_CLASSES`).
    pub const DEFAULT_BLOCKED: &'static [DestinationClass] = &[
        DestinationClass::Premium,
        DestinationClass::SharedCost,
        DestinationClass::Satellite,
        DestinationClass::HighRisk,
    ];
}

/// What happens to a destination no rule classifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnclassifiedPolicy {
    Allow,
    Deny,
}

impl UnclassifiedPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ALLOW" => Some(UnclassifiedPolicy::Allow),
            "DENY" => Some(UnclassifiedPolicy::Deny),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            UnclassifiedPolicy::Allow => "ALLOW",
            UnclassifiedPolicy::Deny => "DENY",
        }
    }
}

/// Outcome of the pre-toll classification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classification {
    /// A rule matched and its class is not blocked.
    Allowed(DestinationClass),
    /// A rule matched a blocked class.
    Blocked(DestinationClass),
    /// No rule matched; the policy decides.
    Unclassified(UnclassifiedPolicy),
}

impl Classification {
    /// Whether the origination may proceed.
    pub fn allowed(&self) -> bool {
        matches!(
            self,
            Classification::Allowed(_) | Classification::Unclassified(UnclassifiedPolicy::Allow)
        )
    }

    /// The bounded label for `telephony_destination_class_refusals_total{class}`.
    pub fn label(&self) -> &'static str {
        match self {
            Classification::Allowed(c) | Classification::Blocked(c) => c.as_str(),
            Classification::Unclassified(_) => "UNCLASSIFIED",
        }
    }
}

/// ITU-T E.164 country calling codes by ISO 3166-1 alpha-2 country (shared codes such
/// as the North American Numbering Plan map every member to the same code).
const COUNTRY_CALLING_CODES: &[(&str, &str)] = &[
    ("AD", "376"),
    ("AE", "971"),
    ("AF", "93"),
    ("AG", "1"),
    ("AI", "1"),
    ("AL", "355"),
    ("AM", "374"),
    ("AO", "244"),
    ("AR", "54"),
    ("AS", "1"),
    ("AT", "43"),
    ("AU", "61"),
    ("AW", "297"),
    ("AX", "358"),
    ("AZ", "994"),
    ("BA", "387"),
    ("BB", "1"),
    ("BD", "880"),
    ("BE", "32"),
    ("BF", "226"),
    ("BG", "359"),
    ("BH", "973"),
    ("BI", "257"),
    ("BJ", "229"),
    ("BL", "590"),
    ("BM", "1"),
    ("BN", "673"),
    ("BO", "591"),
    ("BQ", "599"),
    ("BR", "55"),
    ("BS", "1"),
    ("BT", "975"),
    ("BW", "267"),
    ("BY", "375"),
    ("BZ", "501"),
    ("CA", "1"),
    ("CC", "61"),
    ("CD", "243"),
    ("CF", "236"),
    ("CG", "242"),
    ("CH", "41"),
    ("CI", "225"),
    ("CK", "682"),
    ("CL", "56"),
    ("CM", "237"),
    ("CN", "86"),
    ("CO", "57"),
    ("CR", "506"),
    ("CU", "53"),
    ("CV", "238"),
    ("CW", "599"),
    ("CX", "61"),
    ("CY", "357"),
    ("CZ", "420"),
    ("DE", "49"),
    ("DJ", "253"),
    ("DK", "45"),
    ("DM", "1"),
    ("DO", "1"),
    ("DZ", "213"),
    ("EC", "593"),
    ("EE", "372"),
    ("EG", "20"),
    ("EH", "212"),
    ("ER", "291"),
    ("ES", "34"),
    ("ET", "251"),
    ("FI", "358"),
    ("FJ", "679"),
    ("FK", "500"),
    ("FM", "691"),
    ("FO", "298"),
    ("FR", "33"),
    ("GA", "241"),
    ("GB", "44"),
    ("GD", "1"),
    ("GE", "995"),
    ("GF", "594"),
    ("GG", "44"),
    ("GH", "233"),
    ("GI", "350"),
    ("GL", "299"),
    ("GM", "220"),
    ("GN", "224"),
    ("GP", "590"),
    ("GQ", "240"),
    ("GR", "30"),
    ("GT", "502"),
    ("GU", "1"),
    ("GW", "245"),
    ("GY", "592"),
    ("HK", "852"),
    ("HN", "504"),
    ("HR", "385"),
    ("HT", "509"),
    ("HU", "36"),
    ("ID", "62"),
    ("IE", "353"),
    ("IL", "972"),
    ("IM", "44"),
    ("IN", "91"),
    ("IO", "246"),
    ("IQ", "964"),
    ("IR", "98"),
    ("IS", "354"),
    ("IT", "39"),
    ("JE", "44"),
    ("JM", "1"),
    ("JO", "962"),
    ("JP", "81"),
    ("KE", "254"),
    ("KG", "996"),
    ("KH", "855"),
    ("KI", "686"),
    ("KM", "269"),
    ("KN", "1"),
    ("KP", "850"),
    ("KR", "82"),
    ("KW", "965"),
    ("KY", "1"),
    ("KZ", "7"),
    ("LA", "856"),
    ("LB", "961"),
    ("LC", "1"),
    ("LI", "423"),
    ("LK", "94"),
    ("LR", "231"),
    ("LS", "266"),
    ("LT", "370"),
    ("LU", "352"),
    ("LV", "371"),
    ("LY", "218"),
    ("MA", "212"),
    ("MC", "377"),
    ("MD", "373"),
    ("ME", "382"),
    ("MF", "590"),
    ("MG", "261"),
    ("MH", "692"),
    ("MK", "389"),
    ("ML", "223"),
    ("MM", "95"),
    ("MN", "976"),
    ("MO", "853"),
    ("MP", "1"),
    ("MQ", "596"),
    ("MR", "222"),
    ("MS", "1"),
    ("MT", "356"),
    ("MU", "230"),
    ("MV", "960"),
    ("MW", "265"),
    ("MX", "52"),
    ("MY", "60"),
    ("MZ", "258"),
    ("NA", "264"),
    ("NC", "687"),
    ("NE", "227"),
    ("NF", "672"),
    ("NG", "234"),
    ("NI", "505"),
    ("NL", "31"),
    ("NO", "47"),
    ("NP", "977"),
    ("NR", "674"),
    ("NU", "683"),
    ("NZ", "64"),
    ("OM", "968"),
    ("PA", "507"),
    ("PE", "51"),
    ("PF", "689"),
    ("PG", "675"),
    ("PH", "63"),
    ("PK", "92"),
    ("PL", "48"),
    ("PM", "508"),
    ("PR", "1"),
    ("PS", "970"),
    ("PT", "351"),
    ("PW", "680"),
    ("PY", "595"),
    ("QA", "974"),
    ("RE", "262"),
    ("RO", "40"),
    ("RS", "381"),
    ("RU", "7"),
    ("RW", "250"),
    ("SA", "966"),
    ("SB", "677"),
    ("SC", "248"),
    ("SD", "249"),
    ("SE", "46"),
    ("SG", "65"),
    ("SH", "290"),
    ("SI", "386"),
    ("SJ", "47"),
    ("SK", "421"),
    ("SL", "232"),
    ("SM", "378"),
    ("SN", "221"),
    ("SO", "252"),
    ("SR", "597"),
    ("SS", "211"),
    ("ST", "239"),
    ("SV", "503"),
    ("SX", "1"),
    ("SY", "963"),
    ("SZ", "268"),
    ("TC", "1"),
    ("TD", "235"),
    ("TG", "228"),
    ("TH", "66"),
    ("TJ", "992"),
    ("TK", "690"),
    ("TL", "670"),
    ("TM", "993"),
    ("TN", "216"),
    ("TO", "676"),
    ("TR", "90"),
    ("TT", "1"),
    ("TV", "688"),
    ("TW", "886"),
    ("TZ", "255"),
    ("UA", "380"),
    ("UG", "256"),
    ("US", "1"),
    ("UY", "598"),
    ("UZ", "998"),
    ("VA", "39"),
    ("VC", "1"),
    ("VE", "58"),
    ("VG", "1"),
    ("VI", "1"),
    ("VN", "84"),
    ("VU", "678"),
    ("WF", "681"),
    ("WS", "685"),
    ("XK", "383"),
    ("YE", "967"),
    ("YT", "262"),
    ("ZA", "27"),
    ("ZM", "260"),
    ("ZW", "263"),
];

/// The country calling code of an ISO country, if known.
pub fn country_calling_code(country: &str) -> Option<&'static str> {
    COUNTRY_CALLING_CODES
        .iter()
        .find(|(c, _)| *c == country)
        .map(|(_, code)| *code)
}

/// The national-significant digits of `number` within `country` (the digits after the
/// country calling code), or None when the number does not belong to that country's
/// calling code or the country is unknown.
pub fn national_significant(number: &ExecutionNumber, country: &str) -> Option<String> {
    let code = country_calling_code(country)?;
    number
        .expose()
        .strip_prefix('+')?
        .strip_prefix(code)
        .filter(|rest| !rest.is_empty())
        .map(str::to_string)
}

#[derive(Debug, FromQueryResult)]
struct RuleRow {
    class: String,
}

/// Classifies `destination` in `country`: the longest enabled prefix rule wins. The
/// query carries only the national digits as a parameter (never interpolated, never
/// logged); the result is a bounded class.
pub async fn classify<C: ConnectionTrait>(
    db_: &C,
    country: &str,
    destination: &ExecutionNumber,
    blocked: &[DestinationClass],
    unclassified: UnclassifiedPolicy,
) -> TelephonyResult<Classification> {
    let Some(national) = national_significant(destination, country) else {
        return Ok(Classification::Unclassified(unclassified));
    };
    let row: Option<RuleRow> = db::one(
        db_,
        "SELECT class FROM telecom_destination_classes \
         WHERE country = $1 AND enabled AND left($2, length(prefix)) = prefix \
         ORDER BY length(prefix) DESC LIMIT 1",
        vec![country.into(), national.into()],
    )
    .await?;
    Ok(match row.and_then(|r| DestinationClass::parse(&r.class)) {
        None => Classification::Unclassified(unclassified),
        Some(class) if blocked.contains(&class) => Classification::Blocked(class),
        Some(class) => Classification::Allowed(class),
    })
}

/// Parses `TELEPHONY_BLOCKED_DESTINATION_CLASSES` (comma-separated vocabulary).
pub fn parse_blocked(value: &str) -> Option<Vec<DestinationClass>> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(DestinationClass::parse)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn national_digits_follow_the_country_calling_code() {
        let n = ExecutionNumber::parse("+2348031234567").unwrap();
        assert_eq!(
            national_significant(&n, "NG").as_deref(),
            Some("8031234567")
        );
        // The number is not in the country's plan (or the country is unknown).
        assert!(national_significant(&n, "GB").is_none());
        assert!(national_significant(&n, "ZZ").is_none());
        let us = ExecutionNumber::parse("+12025550123").unwrap();
        assert_eq!(
            national_significant(&us, "US").as_deref(),
            Some("2025550123")
        );
        assert_eq!(
            national_significant(&us, "CA").as_deref(),
            Some("2025550123")
        );
    }

    #[test]
    fn blocked_set_parses_only_the_vocabulary() {
        assert_eq!(
            parse_blocked("PREMIUM, HIGH_RISK").unwrap(),
            vec![DestinationClass::Premium, DestinationClass::HighRisk]
        );
        assert!(parse_blocked("PREMIUM,EXPENSIVE").is_none());
        assert_eq!(parse_blocked("").unwrap(), vec![]);
        assert!(UnclassifiedPolicy::parse("DENY").is_some());
        assert!(UnclassifiedPolicy::parse("deny").is_none());
    }
}
