//! End-to-end tests: feed real markdown through `check_source` and assert
//! on the diagnostic codes produced. These are the tests most resistant to
//! refactors because they exercise the full pipeline.

use td_check::check_source;
use td_core::SourceFile;

fn check(name: &str, src: &str) -> Vec<String> {
    let file = SourceFile::new(name, src.to_string());
    let (_doc, diags) = check_source(&file);
    diags.iter().map(|d| d.code.clone()).collect()
}

const GOOD_PROMPT: &str = r#"---
typedown: Prompt<In, Out>
---

# Reviewer

```td
import { Prompt, Section, Prose, OrderedList, Example } from "typedown/agents"

type In  = { x: string }
type Out = { y: string }

export type Doc = Prompt<In, Out>
```

## Role
Text here.

## Instructions

1. do a thing
2. then another

## Examples

### Example 1
**Input:** x
**Output:** y
"#;

#[test]
fn good_prompt_has_no_diagnostics() {
    let codes = check("good.md", GOOD_PROMPT);
    assert!(codes.is_empty(), "expected no diagnostics, got {codes:?}");
}

#[test]
fn missing_section_fires_td401() {
    // Remove ## Examples.
    let src = GOOD_PROMPT.replace(
        "## Examples\n\n### Example 1\n**Input:** x\n**Output:** y\n",
        "",
    );
    let codes = check("missing.md", &src);
    assert!(codes.contains(&"td401".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_section_fires_td405_warning() {
    let src = format!(
        "{GOOD_PROMPT}\n\n## Unexpected\n\nhmm\n"
    );
    let codes = check("extra.md", &src);
    assert!(codes.contains(&"td405".to_string()), "codes: {codes:?}");
}

#[test]
fn array_section_without_subsections_fires_td402() {
    let src = GOOD_PROMPT.replace(
        "### Example 1\n**Input:** x\n**Output:** y\n",
        "nothing here\n",
    );
    let codes = check("no_subs.md", &src);
    assert!(codes.contains(&"td402".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_type_fires_td403() {
    let src = r#"---
typedown: DoesNotExist
---

# Hi

```td
```
"#;
    let codes = check("unknown.md", src);
    assert!(codes.contains(&"td403".to_string()), "codes: {codes:?}");
}

#[test]
fn unknown_module_fires_td202() {
    let src = r#"---
typedown: Foo
---

# x

```td
import { Foo } from "not/real"
type Doc = { bar: string }
```

## Bar
text
"#;
    let codes = check("bad_import.md", src);
    assert!(codes.contains(&"td202".to_string()), "codes: {codes:?}");
}

#[test]
fn file_without_frontmatter_is_silent() {
    let codes = check("no_fm.md", "# Just a doc\n\nsome text\n");
    assert!(codes.is_empty(), "codes: {codes:?}");
}

#[test]
fn file_with_empty_frontmatter_warns_td301() {
    let codes = check("empty_fm.md", "---\ntitle: foo\n---\n# doc\n");
    assert!(codes.contains(&"td301".to_string()), "codes: {codes:?}");
}
