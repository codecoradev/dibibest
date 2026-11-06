//! DBML optional relationships (`?` next to the operator, dbdiagram Aug 2026)
//! and `<>` refusal (#75). Both used to corrupt the schema silently.

use dibibest_core::{FormatId, Registry};

/// dbdiagram's current sample schema, as pasted into the workspace.
const SAMPLE: &str = r#"
Table follows {
  following_user_id integer [not null]
  followed_user_id integer [not null]
  created_at timestamp
}

Table users {
  id integer [primary key]
  username varchar
  role varchar
  created_at timestamp
}

Table posts {
  id integer [primary key]
  title varchar
  body text [note: 'Content of the post']
  user_id integer [not null]
  status varchar
  created_at timestamp
}

Ref user_posts: posts.user_id ?> users.id // many-to-many

Ref: users.id <? follows.following_user_id

Ref: users.id <? follows.followed_user_id
"#;

fn conv(src: &str, from: &str, to: &str) -> dibibest_core::Conversion {
    Registry::with_defaults()
        .convert_explicit(src, &FormatId::new(from), &FormatId::new(to))
        .unwrap_or_else(|e| panic!("{from} -> {to}: {e}"))
}

#[test]
fn parses_optional_sides_without_touching_names() {
    let ir = Registry::with_defaults()
        .parse(SAMPLE, &FormatId::new("dbml"))
        .unwrap();
    assert_eq!(ir.refs.len(), 3);
    let r = &ir.refs[0];
    assert_eq!(
        (r.from_table.as_str(), r.from_cols[0].as_str()),
        ("posts", "user_id")
    );
    assert_eq!(
        (r.to_table.as_str(), r.to_cols[0].as_str()),
        ("users", "id")
    );
    assert!(r.optional_from && !r.optional_to);
    let r = &ir.refs[1];
    assert_eq!(
        (r.to_table.as_str(), r.to_cols[0].as_str()),
        ("follows", "following_user_id")
    );
    assert!(!r.optional_from && r.optional_to);
    for r in &ir.refs {
        for name in [&r.from_table, &r.to_table]
            .into_iter()
            .chain(&r.from_cols)
            .chain(&r.to_cols)
        {
            assert!(
                !name.contains(['?', '<', '>', ' ']),
                "corrupted name {name:?}"
            );
        }
    }
}

#[test]
fn dbml_round_trip_keeps_every_marker() {
    let c = conv(SAMPLE, "dbml", "dbml");
    assert!(
        c.output
            .contains("Ref user_posts: posts.user_id ?> users.id"),
        "{}",
        c.output
    );
    assert!(c
        .output
        .contains("Ref: users.id <? follows.following_user_id"));
    assert_eq!((c.fidelity.refs.kept, c.fidelity.refs.total), (3, 3));
    assert!(c.fidelity.idempotent);
    assert!(c.fidelity.dropped.iter().all(|d| !d.contains("optional")));
    for src in [
        "Table a {\n  id int [pk]\n}\nTable b {\n  a_id int [ref: ?>? a.id]\n}\n",
        "Table a {\n  id int [pk]\n}\nTable b {\n  a_id int\n}\nRef: b.a_id ?-? a.id\n",
        "Table a {\n  id int [pk]\n}\nTable b {\n  a_id int\n}\nRef r {\n  b.a_id >? a.id [delete: cascade]\n}\n",
    ] {
        let c = conv(src, "dbml", "dbml");
        assert!(c.fidelity.idempotent, "{src}");
        assert_eq!(c.fidelity.refs.kept, 1, "{src}");
        let again = conv(&c.output, "dbml", "dbml");
        assert_eq!(again.output, c.output, "stable round trip for {src}");
    }
}

#[test]
fn erdjson_carries_the_markers() {
    let erd = conv(SAMPLE, "dbml", "erdjson");
    assert!(
        erd.output.contains("\"fromOptional\":true")
            || erd.output.contains("\"fromOptional\": true")
    );
    let back = conv(&erd.output, "erdjson", "dbml");
    assert!(
        back.output.contains("posts.user_id ?> users.id"),
        "{}",
        back.output
    );
}

#[test]
fn sql_and_prisma_report_the_dropped_markers() {
    for to in ["sql", "sql:mysql", "sql:sqlite", "prisma"] {
        let c = conv(SAMPLE, "dbml", to);
        // Prisma writes `?` for nullable fields; SQL must never contain one
        if to != "prisma" {
            assert!(!c.output.contains('?'), "{to}: {}", c.output);
        }
        assert_eq!(
            (c.fidelity.refs.kept, c.fidelity.refs.total),
            (3, 3),
            "{to}"
        );
        let note = c
            .fidelity
            .dropped
            .iter()
            .find(|d| d.contains("optional relationship"))
            .unwrap_or_else(|| panic!("{to}: {:?}", c.fidelity.dropped));
        assert!(note.starts_with("3 "), "{note}");
    }
}

#[test]
fn many_to_many_is_refused_not_rewritten() {
    for op in ["<>", "?<>", "<>?", "?<>?"] {
        let src = format!(
            "Table a {{\n  id int [pk]\n}}\nTable b {{\n  a_id int\n}}\nRef: b.a_id {op} a.id\n"
        );
        let err = Registry::with_defaults()
            .convert_explicit(&src, &FormatId::new("dbml"), &FormatId::new("dbml"))
            .err()
            .unwrap_or_else(|| panic!("{op} must be refused"));
        assert!(err.to_string().contains("many-to-many"), "{op}: {err}");
    }
}

#[test]
fn the_diagram_draws_optional_refs_dashed() {
    use dibibest_core::diagram::{render_source, RenderOptions};
    let svg = render_source(SAMPLE, Some("dbml"), &RenderOptions::default())
        .unwrap()
        .svg;
    assert_eq!(svg.matches("marker-end").count(), 3);
    assert_eq!(svg.matches("stroke-dasharray").count(), 3);
}
