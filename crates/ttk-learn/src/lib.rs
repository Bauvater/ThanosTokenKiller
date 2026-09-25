//! ThanosTokenKiller learned filters.
//!
//! *The filter that nobody has to write.*
//!
//! Every other tool in this space ships hand written filters: somebody notices
//! that `npm install` prints forty deprecation warnings, and somebody writes a
//! regex. That scales exactly as far as the maintainer's patience.
//!
//! This crate inverts it. An agent that has just read a command's output knows
//! better than any maintainer which parts of it were worthless, and it hands
//! that judgement back by wrapping them in a tag:
//!
//! ```text
//! <filter-trash>
//! npm WARN deprecated inflight@1.0.6: This module is not supported
//! </filter-trash>
//! added 412 packages in 9s
//! ```
//!
//! [`annotate::parse`] reads that, [`RuleSet::learn`] generalises it into
//! guarded rules, and [`Engine::filter`] applies them to every later run of the
//! same command. The filter gets better every time somebody uses it, and it is
//! specific to *this* project's real output.
//!
//! # The four things that make it safe
//!
//! 1. **Rules are token templates, not regexes.** A model-written regex is
//!    unreviewable and over-matches; a template is a literal-by-literal
//!    comparison you can read out loud. See [`pattern`].
//! 2. **Unmarked text is a counter-example.** Anything the agent did *not*
//!    mark constrains what may be learned, so over-general rules are rejected
//!    at the moment they are proposed, not after they eat something. See
//!    [`rule::check`].
//! 3. **Errors are never filtered.** The guard reuses the same
//!    `ttk_core::invariants` pass the quality firewall uses, so the filter and
//!    the firewall can never disagree about what counts as evidence.
//! 4. **Nothing is destroyed.** Filtering is an ordinary transformation
//!    candidate: the byte exact original is already in a capsule, and the
//!    firewall reviews the filtered text like any compiler's output.
//!
//! See `docs/learned-filters.md` for the whole design.

pub mod annotate;
pub mod engine;
pub mod pattern;
pub mod rule;
pub mod ruleset;

pub use annotate::{Annotated, FoldGroup};
pub use engine::{Engine, Filtered, RuleHit, record};
pub use pattern::{Class, Template};
pub use rule::{Action, Guards, Rejection, Rule, Scope};
pub use ruleset::{
    LearnOutcome, Lesson, Origin, RULES_FILE, RejectedLine, RuleSet, project_rules_path,
    user_rule_files, user_rules_path,
};

use std::path::Path;

use ttk_core::Result;

/// Project and user rules, merged, with the project taking precedence.
///
/// Loading both layers is the default everywhere: a rule you taught once in one
/// repository should follow you if you stored it globally, and a project's own
/// rules should win when the two ever collide on the same id.
pub struct Layered {
    pub merged: RuleSet,
    pub project: RuleSet,
    pub user: RuleSet,
}

impl Layered {
    pub fn load(workspace_root: &Path) -> Result<Self> {
        let project = RuleSet::load(&project_rules_path(workspace_root))?;
        // Every file in the global filters folder, merged; a rule id defined
        // twice is absorbed into one rule, exactly like `ttk rules import`.
        let mut user = RuleSet::default();
        for path in user_rule_files() {
            user.merge(RuleSet::load(&path)?);
        }
        let mut merged = project.clone();
        merged.merge(user.clone());
        Ok(Self {
            merged,
            project,
            user,
        })
    }

    pub fn len(&self) -> usize {
        self.merged.len()
    }

    pub fn is_empty(&self) -> bool {
        self.merged.is_empty()
    }

    /// Which file a rule id lives in.
    pub fn origin_of(&self, id: &str) -> Option<Origin> {
        if self.project.get(id).is_some() {
            Some(Origin::Project)
        } else if self.user.get(id).is_some() {
            Some(Origin::User)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_workspace_layers_to_an_empty_set() {
        let dir = tempfile::tempdir().expect("tmp");
        let layered = Layered::load(dir.path()).expect("load");
        assert!(layered.project.is_empty());
        assert!(layered.origin_of("flt_nope").is_none());
    }

    #[test]
    fn project_rules_are_found_and_attributed() {
        let dir = tempfile::tempdir().expect("tmp");
        let a = annotate::parse(
            "<filter-trash>\nnpm WARN deprecated pkg@1.0.6 is not supported\n</filter-trash>",
        );
        let mut set = RuleSet::default();
        set.learn(&Lesson::new(&a, Scope::program("npm")));
        let id = set.rules[0].id.clone();
        set.save(&project_rules_path(dir.path())).expect("save");

        let layered = Layered::load(dir.path()).expect("load");
        assert_eq!(layered.project.len(), 1);
        assert_eq!(layered.origin_of(&id), Some(Origin::Project));
        assert!(!layered.is_empty());
    }

    /// The end to end promise of the crate, in one test: teach once, filter
    /// forever, keep the errors.
    #[test]
    fn teach_once_then_every_later_run_is_cleaner() {
        // What the agent saw, with the two worthless lines marked.
        let annotated = "\
<filter-trash>
npm WARN deprecated inflight@1.0.6: This module is not supported
npm WARN deprecated glob@7.2.3: This module is not supported
</filter-trash>
added 412 packages in 9s
";
        let mut set = RuleSet::default();
        let lesson = annotate::parse(annotated);
        let outcome = set.learn(&Lesson::new(&lesson, Scope::command("npm", "install")));
        assert_eq!(outcome.created.len(), 1, "one shape, one rule");

        // A later run with different versions and an added error.
        let later = "\
npm WARN deprecated rimraf@2.7.1: This module is not supported
added 3 packages in 2s
npm ERR! code ELIFECYCLE
";
        let out = Engine::for_command(&set, Some("npm"), Some("install")).filter(later);
        assert_eq!(out.removed_lines, 1);
        assert!(!out.text.contains("deprecated"));
        assert!(out.text.contains("added 3 packages"));
        assert!(out.text.contains("npm ERR!"), "errors always survive");
        assert!(out.tokens_saved().value > 0);
    }
}
