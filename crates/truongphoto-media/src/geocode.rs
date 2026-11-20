use reverse_geocoder::ReverseGeocoder;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const MAX_DISTANCE_KM: f64 = 150.0;

static GEOCODER: OnceLock<ReverseGeocoder> = OnceLock::new();

/// Offline reverse-geocoded location resolved from GeoNames cities data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeoLocation {
    pub city: String,
    pub admin1: String,
    pub country: String,
    pub country_code: String,
}

fn geocoder() -> &'static ReverseGeocoder {
    GEOCODER.get_or_init(ReverseGeocoder::new)
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let r_lat1 = lat1.to_radians();
    let r_lat2 = lat2.to_radians();

    let a = (d_lat / 2.0).sin().powi(2) + r_lat1.cos() * r_lat2.cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    EARTH_RADIUS_KM * c
}

/// Maps ISO 3166-1 alpha-2 codes to clean country names, falling back to the code itself.
pub fn country_name_for_code(code: &str) -> &str {
    match code.trim().to_ascii_uppercase().as_str() {
        "AD" => "Andorra",
        "AE" => "United Arab Emirates",
        "AF" => "Afghanistan",
        "AG" => "Antigua and Barbuda",
        "AI" => "Anguilla",
        "AL" => "Albania",
        "AM" => "Armenia",
        "AO" => "Angola",
        "AQ" => "Antarctica",
        "AR" => "Argentina",
        "AS" => "American Samoa",
        "AT" => "Austria",
        "AU" => "Australia",
        "AW" => "Aruba",
        "AX" => "Aland Islands",
        "AZ" => "Azerbaijan",
        "BA" => "Bosnia and Herzegovina",
        "BB" => "Barbados",
        "BD" => "Bangladesh",
        "BE" => "Belgium",
        "BF" => "Burkina Faso",
        "BG" => "Bulgaria",
        "BH" => "Bahrain",
        "BI" => "Burundi",
        "BJ" => "Benin",
        "BL" => "Saint Barthelemy",
        "BM" => "Bermuda",
        "BN" => "Brunei",
        "BO" => "Bolivia",
        "BQ" => "Bonaire, Sint Eustatius and Saba",
        "BR" => "Brazil",
        "BS" => "Bahamas",
        "BT" => "Bhutan",
        "BV" => "Bouvet Island",
        "BW" => "Botswana",
        "BY" => "Belarus",
        "BZ" => "Belize",
        "CA" => "Canada",
        "CC" => "Cocos Islands",
        "CD" => "Democratic Republic of the Congo",
        "CF" => "Central African Republic",
        "CG" => "Republic of the Congo",
        "CH" => "Switzerland",
        "CI" => "Ivory Coast",
        "CK" => "Cook Islands",
        "CL" => "Chile",
        "CM" => "Cameroon",
        "CN" => "China",
        "CO" => "Colombia",
        "CR" => "Costa Rica",
        "CU" => "Cuba",
        "CV" => "Cabo Verde",
        "CW" => "Curacao",
        "CX" => "Christmas Island",
        "CY" => "Cyprus",
        "CZ" => "Czechia",
        "DE" => "Germany",
        "DJ" => "Djibouti",
        "DK" => "Denmark",
        "DM" => "Dominica",
        "DO" => "Dominican Republic",
        "DZ" => "Algeria",
        "EC" => "Ecuador",
        "EE" => "Estonia",
        "EG" => "Egypt",
        "EH" => "Western Sahara",
        "ER" => "Eritrea",
        "ES" => "Spain",
        "ET" => "Ethiopia",
        "FI" => "Finland",
        "FJ" => "Fiji",
        "FK" => "Falkland Islands",
        "FM" => "Micronesia",
        "FO" => "Faroe Islands",
        "FR" => "France",
        "GA" => "Gabon",
        "GB" => "United Kingdom",
        "GD" => "Grenada",
        "GE" => "Georgia",
        "GF" => "French Guiana",
        "GG" => "Guernsey",
        "GH" => "Ghana",
        "GI" => "Gibraltar",
        "GL" => "Greenland",
        "GM" => "Gambia",
        "GN" => "Guinea",
        "GP" => "Guadeloupe",
        "GQ" => "Equatorial Guinea",
        "GR" => "Greece",
        "GS" => "South Georgia and the South Sandwich Islands",
        "GT" => "Guatemala",
        "GU" => "Guam",
        "GW" => "Guinea-Bissau",
        "GY" => "Guyana",
        "HK" => "Hong Kong",
        "HM" => "Heard Island and McDonald Islands",
        "HN" => "Honduras",
        "HR" => "Croatia",
        "HT" => "Haiti",
        "HU" => "Hungary",
        "ID" => "Indonesia",
        "IE" => "Ireland",
        "IL" => "Israel",
        "IM" => "Isle of Man",
        "IN" => "India",
        "IO" => "British Indian Ocean Territory",
        "IQ" => "Iraq",
        "IR" => "Iran",
        "IS" => "Iceland",
        "IT" => "Italy",
        "JE" => "Jersey",
        "JM" => "Jamaica",
        "JO" => "Jordan",
        "JP" => "Japan",
        "KE" => "Kenya",
        "KG" => "Kyrgyzstan",
        "KH" => "Cambodia",
        "KI" => "Kiribati",
        "KM" => "Comoros",
        "KN" => "Saint Kitts and Nevis",
        "KP" => "North Korea",
        "KR" => "South Korea",
        "KW" => "Kuwait",
        "KY" => "Cayman Islands",
        "KZ" => "Kazakhstan",
        "LA" => "Laos",
        "LB" => "Lebanon",
        "LC" => "Saint Lucia",
        "LI" => "Liechtenstein",
        "LK" => "Sri Lanka",
        "LR" => "Liberia",
        "LS" => "Lesotho",
        "LT" => "Lithuania",
        "LU" => "Luxembourg",
        "LV" => "Latvia",
        "LY" => "Libya",
        "MA" => "Morocco",
        "MC" => "Monaco",
        "MD" => "Moldova",
        "ME" => "Montenegro",
        "MF" => "Saint Martin",
        "MG" => "Madagascar",
        "MH" => "Marshall Islands",
        "MK" => "North Macedonia",
        "ML" => "Mali",
        "MM" => "Myanmar",
        "MN" => "Mongolia",
        "MO" => "Macao",
        "MP" => "Northern Mariana Islands",
        "MQ" => "Martinique",
        "MR" => "Mauritania",
        "MS" => "Montserrat",
        "MT" => "Malta",
        "MU" => "Mauritius",
        "MV" => "Maldives",
        "MW" => "Malawi",
        "MX" => "Mexico",
        "MY" => "Malaysia",
        "MZ" => "Mozambique",
        "NA" => "Namibia",
        "NC" => "New Caledonia",
        "NE" => "Niger",
        "NF" => "Norfolk Island",
        "NG" => "Nigeria",
        "NI" => "Nicaragua",
        "NL" => "Netherlands",
        "NO" => "Norway",
        "NP" => "Nepal",
        "NR" => "Nauru",
        "NU" => "Niue",
        "NZ" => "New Zealand",
        "OM" => "Oman",
        "PA" => "Panama",
        "PE" => "Peru",
        "PF" => "French Polynesia",
        "PG" => "Papua New Guinea",
        "PH" => "Philippines",
        "PK" => "Pakistan",
        "PL" => "Poland",
        "PM" => "Saint Pierre and Miquelon",
        "PN" => "Pitcairn Islands",
        "PR" => "Puerto Rico",
        "PS" => "Palestine",
        "PT" => "Portugal",
        "PW" => "Palau",
        "PY" => "Paraguay",
        "QA" => "Qatar",
        "RE" => "Reunion",
        "RO" => "Romania",
        "RS" => "Serbia",
        "RU" => "Russia",
        "RW" => "Rwanda",
        "SA" => "Saudi Arabia",
        "SB" => "Solomon Islands",
        "SC" => "Seychelles",
        "SD" => "Sudan",
        "SE" => "Sweden",
        "SG" => "Singapore",
        "SH" => "Saint Helena",
        "SI" => "Slovenia",
        "SJ" => "Svalbard and Jan Mayen",
        "SK" => "Slovakia",
        "SL" => "Sierra Leone",
        "SM" => "San Marino",
        "SN" => "Senegal",
        "SO" => "Somalia",
        "SR" => "Suriname",
        "SS" => "South Sudan",
        "ST" => "Sao Tome and Principe",
        "SV" => "El Salvador",
        "SX" => "Sint Maarten",
        "SY" => "Syria",
        "SZ" => "Eswatini",
        "TC" => "Turks and Caicos Islands",
        "TD" => "Chad",
        "TF" => "French Southern Territories",
        "TG" => "Togo",
        "TH" => "Thailand",
        "TJ" => "Tajikistan",
        "TK" => "Tokelau",
        "TL" => "Timor-Leste",
        "TM" => "Turkmenistan",
        "TN" => "Tunisia",
        "TO" => "Tonga",
        "TR" => "Turkey",
        "TT" => "Trinidad and Tobago",
        "TV" => "Tuvalu",
        "TW" => "Taiwan",
        "TZ" => "Tanzania",
        "UA" => "Ukraine",
        "UG" => "Uganda",
        "UM" => "U.S. Minor Outlying Islands",
        "US" => "United States",
        "UY" => "Uruguay",
        "UZ" => "Uzbekistan",
        "VA" => "Vatican City",
        "VC" => "Saint Vincent and the Grenadines",
        "VE" => "Venezuela",
        "VG" => "British Virgin Islands",
        "VI" => "U.S. Virgin Islands",
        "VN" => "Vietnam",
        "VU" => "Vanuatu",
        "WF" => "Wallis and Futuna",
        "WS" => "Samoa",
        "XK" => "Kosovo",
        "YE" => "Yemen",
        "YT" => "Mayotte",
        "ZA" => "South Africa",
        "ZM" => "Zambia",
        "ZW" => "Zimbabwe",
        _ => code,
    }
}

/// Resolves `(lat, lon)` offline via the embedded GeoNames database, enforcing a 150 km guard.
pub fn reverse_geocode(lat: f64, lon: f64) -> Option<GeoLocation> {
    if !lat.is_finite()
        || !lon.is_finite()
        || !(-90.0..=90.0).contains(&lat)
        || !(-180.0..=180.0).contains(&lon)
    {
        return None;
    }

    let result = geocoder().search((lat, lon));
    let distance_km = haversine_km(lat, lon, result.record.lat, result.record.lon);
    if distance_km > MAX_DISTANCE_KM {
        return None;
    }

    let country_code = result.record.cc.trim().to_ascii_uppercase();
    let country = if country_code.is_empty() {
        String::new()
    } else {
        let mapped = country_name_for_code(&country_code);
        if mapped.eq_ignore_ascii_case(&country_code) {
            country_code.clone()
        } else {
            mapped.to_string()
        }
    };

    Some(GeoLocation {
        city: result.record.name.clone(),
        admin1: result.record.admin1.clone(),
        country,
        country_code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geocodes_tokyo_and_rejects_mid_ocean() {
        let tokyo = reverse_geocode(35.6762, 139.6503).expect("Tokyo should resolve");
        assert_eq!(tokyo.country_code, "JP");
        assert_eq!(tokyo.country, "Japan");
        assert!(!tokyo.city.is_empty());

        // Mid-Pacific Point Nemo (>2600 km from nearest inhabited land)
        assert!(reverse_geocode(-48.8767, -123.3933).is_none());
        assert!(reverse_geocode(f64::NAN, 0.0).is_none());
    }
}
