//! Deliberate channel renaming and unit conversion.
//!
//! # Why this is opt-in
//!
//! Reading telemetry losslessly and presenting it in an analysis tool's
//! preferred names and units are opposing goals. The readers therefore report
//! exactly what a file contains (see `UnitSource`), and any renaming or
//! rescaling happens here, only when the caller asks for it.
//!
//! This split matters because real files disagree with each other in ways no
//! automatic mapping can resolve. In one ORECA 07 export `gear` reads 1..6
//! while `gear_pos` reads 2..7 for the same physical quantity, and the
//! `FIA_Gps*` channels are present but flat zero. An automatic mapper would
//! have to pick one and would sometimes produce a plausible-looking but wrong
//! trace. An explicit map makes that choice visible and reviewable.
//!
//! # Format
//!
//! A map is a list of rules, one per line, either as a `VARCHAR` argument or
//! read from a file:
//!
//! ```text
//! # comment
//! source_channel -> target_name [unit] [*scale] [+offset]
//! ```
//!
//! Only the source channel is required. Examples:
//!
//! ```text
//! Speed_Ref     -> Ground Speed  [km/h] *3.6      # m/s -> km/h
//! STEER         -> Steered Angle [deg]  *57.29578 # rad -> deg
//! P_F_BRAKE     -> Brake Press   [bar]  *0.00001  # Pa  -> bar
//! I_ACCEL_LONG  -> G Force Long  [g]    *0.101972 # m/s^2 -> g
//! ACT           -> Air Temp      [C]    +-273.15  # K   -> C
//! gear_pos      -> Gear                 +-1       # 2..7 -> 1..6
//! ```
//!
//! Conversion is `value * scale + offset`, applied in that order.

use motorsport_telemetry_core::{units, UnitSource};
use std::collections::HashMap;
use std::error::Error;

/// One channel's rename and/or conversion.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRule {
    /// Channel name to match, compared case-insensitively.
    pub source: String,
    /// Replacement name, when renaming.
    pub target: Option<String>,
    /// Replacement unit, when converting or relabelling.
    pub unit: Option<String>,
    /// Multiplier applied to every sample.
    pub scale: f64,
    /// Added after scaling.
    pub offset: f64,
}

impl ChannelRule {
    /// Whether this rule changes sample values at all.
    pub fn is_identity(&self) -> bool {
        self.scale == 1.0 && self.offset == 0.0
    }

    /// Apply the conversion to one sample.
    #[inline]
    pub fn convert(&self, value: f64) -> f64 {
        if self.is_identity() {
            value
        } else {
            value * self.scale + self.offset
        }
    }
}

/// A parsed set of rules, indexed by lowercased source channel name.
#[derive(Debug, Clone, Default)]
pub struct ChannelMap {
    rules: HashMap<String, ChannelRule>,
}

impl ChannelMap {
    /// True when no rules are present, so callers can skip all mapping work.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Look up the rule for a channel name, if any.
    pub fn rule_for(&self, channel: &str) -> Option<&ChannelRule> {
        self.rules.get(&channel.to_ascii_lowercase())
    }

    /// The mapped name for a channel, or the original when unmapped.
    pub fn name_for<'a>(&'a self, channel: &'a str) -> &'a str {
        match self
            .rule_for(channel)
            .and_then(|rule| rule.target.as_deref())
        {
            Some(target) => target,
            None => channel,
        }
    }

    /// The mapped unit and its provenance.
    ///
    /// A unit supplied by a map is [`UnitSource::Declared`]: the caller stated
    /// it explicitly, which is the strongest provenance available.
    pub fn unit_for(&self, channel: &str, unit: &str, source: UnitSource) -> (String, UnitSource) {
        match self.rule_for(channel).and_then(|rule| rule.unit.as_deref()) {
            Some(mapped) => (mapped.to_owned(), UnitSource::Declared),
            None => (unit.to_owned(), source),
        }
    }

    /// Parse rules from text. Blank lines and `#` comments are ignored.
    ///
    /// Returns an error rather than skipping a malformed line: a silently
    /// dropped rule would mean silently unconverted data.
    pub fn parse(text: &str) -> Result<Self, Box<dyn Error>> {
        let mut rules = HashMap::new();
        for (index, raw) in text.lines().enumerate() {
            let line = match raw.split_once('#') {
                Some((before, _)) => before.trim(),
                None => raw.trim(),
            };
            if line.is_empty() {
                continue;
            }
            let rule = Self::parse_rule(line)
                .map_err(|error| format!("channel_map line {}: {error}", index + 1))?;
            let key = rule.source.to_ascii_lowercase();
            if rules.contains_key(&key) {
                return Err(format!(
                    "channel_map line {}: duplicate rule for '{}'",
                    index + 1,
                    rule.source
                )
                .into());
            }
            rules.insert(key, rule);
        }
        Ok(Self { rules })
    }

    fn parse_rule(line: &str) -> Result<ChannelRule, Box<dyn Error>> {
        // Split source from the rest at the arrow, if present.
        let (source, rest) = match line.split_once("->") {
            Some((source, rest)) => (source.trim(), rest.trim()),
            None => (line.trim(), ""),
        };
        if source.is_empty() {
            return Err("missing source channel name".into());
        }

        let mut target_words: Vec<&str> = Vec::new();
        let mut unit = None;
        let mut scale = 1.0f64;
        let mut offset = 0.0f64;

        // The unit is bracketed so target names may contain spaces.
        let rest = if let Some(open) = rest.find('[') {
            let close = rest[open..]
                .find(']')
                .ok_or("unterminated '[' in unit field")?
                + open;
            unit = Some(rest[open + 1..close].trim().to_owned());
            let mut merged = rest[..open].to_owned();
            merged.push(' ');
            merged.push_str(&rest[close + 1..]);
            merged
        } else {
            rest.to_owned()
        };

        for token in rest.split_whitespace() {
            if let Some(value) = token.strip_prefix('*') {
                scale = value
                    .parse()
                    .map_err(|_| format!("invalid scale '{value}'"))?;
            } else if let Some(value) = token.strip_prefix('+') {
                offset = value
                    .parse()
                    .map_err(|_| format!("invalid offset '{value}'"))?;
            } else {
                target_words.push(token);
            }
        }
        if !scale.is_finite() || !offset.is_finite() {
            return Err("scale and offset must be finite".into());
        }
        if scale == 0.0 {
            return Err("scale of 0 would discard the channel's data".into());
        }

        let target = if target_words.is_empty() {
            None
        } else {
            Some(target_words.join(" "))
        };
        Ok(ChannelRule {
            source: source.to_owned(),
            target,
            unit,
            scale,
            offset,
        })
    }

    /// Validate that every rule matches a channel that exists.
    ///
    /// A typo in a map would otherwise silently do nothing, which is exactly
    /// the failure mode this crate is trying to avoid.
    pub fn validate(&self, available: &[String]) -> Result<(), Box<dyn Error>> {
        let present: std::collections::HashSet<String> =
            available.iter().map(|n| n.to_ascii_lowercase()).collect();
        let mut missing: Vec<&str> = self
            .rules
            .iter()
            .filter(|(key, _)| !present.contains(*key))
            .map(|(_, rule)| rule.source.as_str())
            .collect();
        if !missing.is_empty() {
            missing.sort_unstable();
            return Err(format!(
                "channel_map references channel(s) not present in the data: {}",
                missing.join(", ")
            )
            .into());
        }
        Ok(())
    }
}

/// Build `COMMENT ON COLUMN` statements documenting each column's unit.
///
/// DuckDB cannot attach comments to a table function's result columns, so a
/// query that wants unit metadata persisted has to materialise a table first.
/// This generates the DDL for that, which is why `telemetry_column_comments`
/// exists as a companion to the readers.
///
/// `columns` pairs a SQL column name with its unit and provenance. Channels
/// with no unit are skipped rather than commented with an empty string.
/// The unit metadata payload attached to a column, in both carriers.
///
/// Two carriers are needed because neither alone is sufficient:
///
/// - `COMMENT ON COLUMN` lives in the DuckDB catalog and is queryable via
///   `duckdb_columns()`, but is **lost by `COPY ... TO 'x.parquet'`**.
/// - Parquet `KV_METADATA` survives export to a file and travels to other
///   tools, but cannot be attached to a DuckDB table.
///
/// Emitting both means units survive whichever way the data leaves.
pub fn unit_payload(unit: &str, source: UnitSource) -> String {
    let mut payload = format!("unit={unit}; source={}", source.name());
    if let Some(def) = units::lookup(unit) {
        // Canonical spelling and dimension make the metadata self-describing:
        // a downstream reader can normalise and dimension-check without
        // needing this registry.
        if def.canonical != unit {
            payload.push_str(&format!("; canonical={}", def.canonical));
        }
        payload.push_str(&format!(
            "; dimension={}; base_unit={}",
            def.dimension.name(),
            def.dimension.base_unit()
        ));
    }
    payload
}

/// Generate `COMMENT ON COLUMN` statements carrying each column's unit.
pub fn column_comment_ddl(table: &str, columns: &[(String, String, UnitSource)]) -> Vec<String> {
    columns
        .iter()
        .filter(|(_, unit, _)| !unit.is_empty())
        .map(|(column, unit, source)| {
            format!(
                "COMMENT ON COLUMN {} IS '{}';",
                quote_qualified(table, column),
                escape_literal(&unit_payload(unit, *source))
            )
        })
        .collect()
}

/// Quote a table and column name for SQL, handling embedded quotes.
fn quote_qualified(table: &str, column: &str) -> String {
    // The table may already be schema-qualified, so quote each part.
    let table = table
        .split('.')
        .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(".");
    format!("{table}.\"{}\"", column.replace('"', "\"\""))
}

/// Escape a single-quoted SQL string literal.
fn escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_rule() {
        let map = ChannelMap::parse("Speed_Ref -> Ground Speed [km/h] *3.6").unwrap();
        let rule = map.rule_for("speed_ref").expect("rule");
        assert_eq!(rule.target.as_deref(), Some("Ground Speed"));
        assert_eq!(rule.unit.as_deref(), Some("km/h"));
        assert_eq!(rule.scale, 3.6);
        assert_eq!(rule.offset, 0.0);
    }

    #[test]
    fn converts_the_real_world_cases_correctly() {
        // These are the exact conversions Cosworth SI data needs for i2.
        let map = ChannelMap::parse(
            "
            Speed_Ref    -> Ground Speed  [km/h] *3.6
            STEER        -> Steered Angle [deg]  *57.29577951308232
            P_F_BRAKE    -> Brake Press   [bar]  *0.00001
            I_ACCEL_LONG -> G Force Long  [g]    *0.10197162129779283
            ACT          -> Air Temp      [C]    +-273.15
            X_FL_DAMPER  -> Damper FL     [mm]   *1000
            ",
        )
        .unwrap();

        // 74.75 m/s is the observed top speed: 269.1 km/h.
        let speed = map.rule_for("Speed_Ref").unwrap();
        assert!((speed.convert(74.75) - 269.1).abs() < 1e-9);

        // -2.5831 rad is the observed steering extreme: about -148 deg.
        let steer = map.rule_for("STEER").unwrap();
        assert!((steer.convert(-2.5831) + 148.0).abs() < 0.1);

        // 7_515_500 Pa is the observed peak brake pressure: 75.155 bar.
        let brake = map.rule_for("P_F_BRAKE").unwrap();
        assert!((brake.convert(7_515_500.0) - 75.155).abs() < 1e-6);

        // -29.8122 m/s^2 is the observed peak braking: about -3.04 g.
        let accel = map.rule_for("I_ACCEL_LONG").unwrap();
        assert!((accel.convert(-29.8122) + 3.04).abs() < 0.01);

        // 300 K is 26.85 C.
        let temp = map.rule_for("ACT").unwrap();
        assert!((temp.convert(300.0) - 26.85).abs() < 1e-9);

        // 0.0182 m is 18.2 mm.
        let damper = map.rule_for("X_FL_DAMPER").unwrap();
        assert!((damper.convert(0.0182) - 18.2).abs() < 1e-9);
    }

    #[test]
    fn handles_offset_only_rules() {
        // gear_pos reads 2..7 for the same gears `gear` reports as 1..6.
        let map = ChannelMap::parse("gear_pos -> Gear +-1").unwrap();
        let rule = map.rule_for("gear_pos").unwrap();
        assert_eq!(rule.convert(2.0), 1.0);
        assert_eq!(rule.convert(7.0), 6.0);
        assert_eq!(rule.unit, None);
    }

    #[test]
    fn rename_only_and_unit_only_rules_are_identity() {
        let map = ChannelMap::parse(
            "
            uSpeed -> Vehicle Speed
            TPS    -> [%]
            ",
        )
        .unwrap();
        let rename = map.rule_for("uSpeed").unwrap();
        assert!(rename.is_identity());
        assert_eq!(rename.convert(12.5), 12.5);

        let relabel = map.rule_for("TPS").unwrap();
        assert!(relabel.is_identity());
        assert_eq!(relabel.unit.as_deref(), Some("%"));
        assert_eq!(relabel.target, None);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let map = ChannelMap::parse(
            "
            # leading comment

            STEER -> Angle [deg] *57.29578  # trailing comment
            ",
        )
        .unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.rule_for("steer").unwrap().unit.as_deref(), Some("deg"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let map = ChannelMap::parse("speed_ref -> Speed [km/h] *3.6").unwrap();
        assert!(map.rule_for("SPEED_REF").is_some());
        assert!(map.rule_for("Speed_Ref").is_some());
    }

    #[test]
    fn mapped_units_are_reported_as_declared() {
        let map = ChannelMap::parse("STEER -> Angle [deg] *57.29578").unwrap();
        let (unit, source) = map.unit_for("STEER", "rad", UnitSource::SpecDefault);
        assert_eq!(unit, "deg");
        assert_eq!(source, UnitSource::Declared);
    }

    #[test]
    fn unmapped_channels_keep_their_unit_and_provenance() {
        let map = ChannelMap::parse("STEER -> Angle [deg] *57.29578").unwrap();
        let (unit, source) = map.unit_for("Speed_Ref", "m/s", UnitSource::SpecDefault);
        assert_eq!(unit, "m/s");
        assert_eq!(source, UnitSource::SpecDefault);
        assert_eq!(map.name_for("Speed_Ref"), "Speed_Ref");
    }

    #[test]
    fn rejects_malformed_rules_instead_of_skipping_them() {
        // A silently dropped rule means silently unconverted data.
        assert!(ChannelMap::parse("STEER -> Angle [deg] *notanumber").is_err());
        assert!(ChannelMap::parse("STEER -> Angle [deg *2").is_err());
        assert!(ChannelMap::parse("-> Angle").is_err());
        assert!(ChannelMap::parse("STEER -> Angle *0").is_err());
        // Duplicates are ambiguous, so they are an error too.
        assert!(ChannelMap::parse("A -> B\nA -> C").is_err());
    }

    #[test]
    fn validate_catches_typos_in_channel_names() {
        let map = ChannelMap::parse("Speed_Reff -> Speed [km/h] *3.6").unwrap();
        let available = vec!["Speed_Ref".to_owned(), "STEER".to_owned()];
        let error = map.validate(&available).expect_err("typo must be caught");
        assert!(error.to_string().contains("Speed_Reff"));

        let good = ChannelMap::parse("Speed_Ref -> Speed [km/h] *3.6").unwrap();
        assert!(good.validate(&available).is_ok());
    }

    #[test]
    fn generates_column_comment_ddl() {
        let columns = vec![
            (
                "Ground Speed".to_owned(),
                "km/h".to_owned(),
                UnitSource::Declared,
            ),
            (
                "STEER".to_owned(),
                "rad".to_owned(),
                UnitSource::SpecDefault,
            ),
            // No unit: must be skipped, not commented with an empty string.
            ("gear".to_owned(), String::new(), UnitSource::Unknown),
        ];
        let ddl = column_comment_ddl("laps", &columns);
        assert_eq!(ddl.len(), 2, "unitless channels are skipped");
        assert_eq!(
            ddl[0],
            "COMMENT ON COLUMN \"laps\".\"Ground Speed\" IS 'unit=km/h; source=declared; dimension=speed; base_unit=m/s';"
        );
        assert_eq!(
            ddl[1],
            "COMMENT ON COLUMN \"laps\".\"STEER\" IS 'unit=rad; source=spec_default; dimension=angle; base_unit=rad';"
        );
    }

    #[test]
    fn ddl_quoting_survives_hostile_identifiers() {
        let columns = vec![(
            "we\"ird".to_owned(),
            "m'/s".to_owned(),
            UnitSource::Declared,
        )];
        let ddl = column_comment_ddl("my.sch\"ema", &columns);
        // Identifiers double their quotes; literals double their apostrophes.
        assert_eq!(
            ddl[0],
            "COMMENT ON COLUMN \"my\".\"sch\"\"ema\".\"we\"\"ird\" IS 'unit=m''/s; source=declared';"
        );
    }
}
