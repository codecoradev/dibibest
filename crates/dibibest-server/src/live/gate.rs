//! The gate (spike, issue #88): the one function every change passes, whatever it came in through.
//!
//! It sees the text a change would leave and decides. A person's edit is always accepted unless it breaks a
//! structural limit: the editor must keep working while the text is mid-edit, and the canvas keeps showing the
//! last valid schema (existing behaviour). An agent's edit must leave a schema that parses; the refusal names
//! the parse problem so the agent can fix it. Role, quota and risk tier checks would sit here too.

use dibibest_core::{default_registry, FormatId};

/// Largest text a project may hold, in UTF-16 units. A placeholder for the per-project quota.
pub const MAX_TEXT_UNITS: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub name: String,
    pub kind: ActorKind,
}

impl Actor {
    pub fn human(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: ActorKind::Human,
        }
    }
    pub fn agent(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: ActorKind::Agent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct Refusal {
    pub message: String,
}

/// Decides whether `actor` may leave the project with the text `text()` gives, which has `units` UTF-16 units.
/// The text is only built when the actor's kind needs to read it: a person's edit is never parsed here.
pub fn gate(actor: &Actor, units: usize, text: impl FnOnce() -> String) -> Result<(), Refusal> {
    if units > MAX_TEXT_UNITS {
        return Err(Refusal {
            message: format!("the text would be {units} units, the limit is {MAX_TEXT_UNITS}"),
        });
    }
    match actor.kind {
        ActorKind::Human => Ok(()),
        ActorKind::Agent => default_registry()
            .parse(&text(), &FormatId::new("dbml"))
            .map(|_| ())
            .map_err(|e| Refusal {
                message: format!("the result is not valid DBML: {e}"),
            }),
    }
}
