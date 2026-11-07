use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use globset::{Glob, GlobSetBuilder};
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use walkdir::WalkDir;

const THEME_CONTENT_PATH: &str = "crates/settings_content/src/theme.rs";
const SYNTAX_CORE_PATH: &str = "docs/src/extensions/languages.md";
const BACKGROUND_APPEARANCE_KEY: &str = "background.appearance";
const BACKGROUND_APPEARANCE_VALUES: [&str; 5] = [
    "opaque",
    "transparent",
    "blurred",
    "mica_backdrop",
    "mica_alt_backdrop",
];

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Check Zed theme family files for key coverage",
    long_about = "Validate Zed theme family files (themes[].style) against available non-syntax and syntax keys, and check that color values parse.",
    after_help = "Examples:\n  script/check-theme-keys --theme path/to/theme.json\n  script/check-theme-keys --theme-glob \"assets/themes/**/*.json\" --syntax-level both --format json\n  script/check-theme-keys --theme themes/my.json --strict --fail-on unknown,uncovered,deprecated\n\nExit codes:\n  0 = pass\n  1 = policy violation (with --strict)\n  2 = runtime or parse error"
)]
struct Args {
    /// Path to a theme family JSON/JSONC file.
    #[arg(long = "theme")]
    themes: Vec<PathBuf>,

    /// Glob pattern (repo-root relative) for theme family files.
    #[arg(long = "theme-glob")]
    theme_globs: Vec<String>,

    /// Which syntax key set to validate against.
    #[arg(long, value_enum, default_value_t = SyntaxLevel::Off)]
    syntax_level: SyntaxLevel,

    /// Return exit code 1 when configured failure kinds are present.
    #[arg(long)]
    strict: bool,

    /// Comma-separated failure kinds: uncovered,unknown,deprecated.
    ///
    /// `deprecated` implies `--deprecated include`.
    #[arg(long, default_value = "unknown,uncovered")]
    fail_on: String,

    /// Include or exclude deprecated keys from reporting/policy checks.
    #[arg(long, value_enum, default_value_t = DeprecatedMode::Exclude)]
    deprecated: DeprecatedMode,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,

    /// Emit GitHub Actions annotations (warning/error lines).
    #[arg(long)]
    github_annotations: bool,

    /// Repository root where Zed source files are read from.
    #[arg(long, default_value = ".")]
    repo_root: PathBuf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SyntaxLevel {
    Off,
    Core,
    Observed,
    Both,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DeprecatedMode {
    Include,
    Exclude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum FailKind {
    Uncovered,
    Unknown,
    Deprecated,
}

#[derive(Debug)]
struct KeySets {
    expected_non_syntax: BTreeSet<String>,
    deprecated_non_syntax: BTreeSet<String>,
    syntax_core: BTreeSet<String>,
    syntax_observed: BTreeSet<String>,
}

#[derive(Debug, Serialize)]
struct FileReport {
    path: String,
    themes: Vec<ThemeReport>,
}

#[derive(Debug, Serialize)]
struct ThemeReport {
    theme_name: String,
    missing_non_syntax: Vec<String>,
    unknown_non_syntax: Vec<String>,
    deprecated_non_syntax: Vec<String>,
    syntax_uncovered: Vec<String>,
    syntax_covered_by_ancestor: Vec<AncestorCoverage>,
    syntax_exact_count: usize,
}

#[derive(Debug, Serialize)]
struct AncestorCoverage {
    capture: String,
    covered_by: String,
}

#[derive(Debug, Serialize)]
struct SummaryReport {
    files: Vec<FileReport>,
    total_missing_non_syntax: usize,
    total_unknown_non_syntax: usize,
    total_deprecated_non_syntax: usize,
    total_syntax_uncovered: usize,
    total_syntax_covered_by_ancestor: usize,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err:#}");
            2
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let args = Args::parse();
    let repo_root = fs::canonicalize(&args.repo_root)
        .with_context(|| format!("failed to resolve repo root: {}", args.repo_root.display()))?;

    let theme_files = resolve_theme_files(&repo_root, &args.themes, &args.theme_globs)?;
    let key_sets = load_key_sets(&repo_root)?;
    let fail_kinds = parse_fail_kinds(&args.fail_on)?;

    // Deprecated keys are only reported in include mode, so `--fail-on deprecated` would
    // otherwise never fire.
    let deprecated_mode = if fail_kinds.contains(&FailKind::Deprecated) {
        DeprecatedMode::Include
    } else {
        args.deprecated
    };

    let syntax_expected = expected_syntax_keys(&key_sets, args.syntax_level);

    let mut file_reports = Vec::new();
    for path in theme_files {
        let report = analyze_theme_file(
            &repo_root,
            &path,
            &key_sets,
            &syntax_expected,
            deprecated_mode,
        )?;
        file_reports.push(report);
    }

    let summary = summarize(file_reports);

    match args.format {
        OutputFormat::Text => print_text_report(&summary),
        OutputFormat::Json => {
            let output = serde_json::to_string_pretty(&summary)?;
            println!("{output}");
        }
    }

    if args.github_annotations {
        print_github_annotations(&summary);
    }

    if args.strict && has_policy_violations(&summary, &fail_kinds) {
        return Ok(1);
    }

    Ok(0)
}

fn resolve_theme_files(
    repo_root: &Path,
    explicit: &[PathBuf],
    globs: &[String],
) -> Result<Vec<PathBuf>> {
    if explicit.is_empty() && globs.is_empty() {
        bail!("provide at least one --theme or --theme-glob");
    }

    let mut result = BTreeSet::new();

    for path in explicit {
        let absolute = if path.is_absolute() {
            path.clone()
        } else {
            repo_root.join(path)
        };

        if !absolute.exists() {
            bail!("theme file does not exist: {}", absolute.display());
        }
        result.insert(absolute);
    }

    if !globs.is_empty() {
        let mut builder = GlobSetBuilder::new();
        for pattern in globs {
            if Path::new(pattern).is_absolute() {
                bail!("absolute --theme-glob is not supported: {pattern}");
            }
            builder.add(Glob::new(pattern).with_context(|| format!("invalid glob: {pattern}"))?);
        }
        let set = builder.build()?;

        for entry in WalkDir::new(repo_root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }

            let Ok(rel) = entry.path().strip_prefix(repo_root) else {
                continue;
            };
            if set.is_match(rel) {
                result.insert(entry.path().to_path_buf());
            }
        }
    }

    if result.is_empty() {
        bail!("no theme files matched");
    }

    Ok(result.into_iter().collect())
}

fn load_key_sets(repo_root: &Path) -> Result<KeySets> {
    let theme_content = fs::read_to_string(repo_root.join(THEME_CONTENT_PATH))
        .with_context(|| format!("failed to read {}", THEME_CONTENT_PATH))?;
    let syntax_core_md = fs::read_to_string(repo_root.join(SYNTAX_CORE_PATH))
        .with_context(|| format!("failed to read {}", SYNTAX_CORE_PATH))?;

    let theme_style_block = extract_struct_block(&theme_content, "ThemeStyleContent")?;
    let theme_colors_block = extract_struct_block(&theme_content, "ThemeColorsContent")?;
    let status_colors_block = extract_struct_block(&theme_content, "StatusColorsContent")?;

    let mut deprecated_non_syntax = extract_deprecated_keys(theme_colors_block)?;
    deprecated_non_syntax.extend(extract_deprecated_keys(status_colors_block)?);
    deprecated_non_syntax.extend(extract_deprecated_keys(theme_style_block)?);

    let mut expected_non_syntax = BTreeSet::new();
    expected_non_syntax.extend(extract_serde_renames(theme_style_block)?);
    expected_non_syntax.extend(extract_serde_renames(theme_colors_block)?);
    expected_non_syntax.extend(extract_serde_renames(status_colors_block)?);
    expected_non_syntax.retain(|key| !deprecated_non_syntax.contains(key));

    let syntax_core = extract_core_syntax_keys(&syntax_core_md)?;
    let syntax_observed = extract_observed_syntax_keys(repo_root)?;

    // An empty set means the source layout changed and the check would silently pass.
    if expected_non_syntax.is_empty() {
        bail!(
            "found no theme style keys in {THEME_CONTENT_PATH}; the schema layout probably changed"
        );
    }
    if syntax_core.is_empty() {
        bail!("found no syntax captures under '### Syntax highlighting' in {SYNTAX_CORE_PATH}");
    }
    if syntax_observed.is_empty() {
        bail!("found no syntax captures in any highlights.scm under the repo root");
    }

    Ok(KeySets {
        expected_non_syntax,
        deprecated_non_syntax,
        syntax_core,
        syntax_observed,
    })
}

fn extract_struct_block<'a>(content: &'a str, struct_name: &str) -> Result<&'a str> {
    let marker = format!("pub struct {struct_name}");
    let start = content
        .find(&marker)
        .ok_or_else(|| anyhow!("could not find struct {struct_name}"))?;
    let brace_start = content[start..]
        .find('{')
        .map(|idx| start + idx)
        .ok_or_else(|| anyhow!("could not find opening brace for struct {struct_name}"))?;

    let mut depth = 0usize;
    let mut end = None;
    for (idx, ch) in content[brace_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    end = Some(brace_start + idx + 1);
                    break;
                }
            }
            _ => {}
        }
    }

    let end =
        end.ok_or_else(|| anyhow!("could not find closing brace for struct {struct_name}"))?;
    Ok(&content[start..end])
}

fn extract_serde_renames(block: &str) -> Result<BTreeSet<String>> {
    let regex = Regex::new(r#"#\[serde\(rename = "([^"]+)"(?:, [^\)]*)?\)\]"#)?;
    Ok(regex
        .captures_iter(block)
        .map(|caps| caps[1].to_string())
        .collect())
}

fn extract_deprecated_keys(block: &str) -> Result<BTreeSet<String>> {
    let rename_regex = Regex::new(r#"#\[serde\(rename = "([^"]+)"(?:, [^\)]*)?\)\]"#)?;
    let field_regex = Regex::new(r"^pub\s+([a-zA-Z0-9_]+)\s*:")?;

    let mut pending_rename: Option<String> = None;
    let mut pending_deprecated_attr = false;
    let mut pending_deprecated_doc = false;
    let mut previous_line_was_doc = false;
    let mut deprecated = BTreeSet::new();

    for line in block.lines() {
        let trimmed = line.trim();

        if let Some(caps) = rename_regex.captures(trimmed) {
            pending_rename = Some(caps[1].to_string());
            previous_line_was_doc = false;
            continue;
        }

        if trimmed == "#[deprecated]" {
            pending_deprecated_attr = true;
            previous_line_was_doc = false;
            continue;
        }

        if trimmed.starts_with("///") {
            // Only a doc block that opens with "Deprecated" marks the field, so prose such as
            // "replaces the deprecated foo color" on a live field does not.
            if !previous_line_was_doc {
                pending_deprecated_doc = false;
            }
            previous_line_was_doc = true;
            if trimmed
                .trim_start_matches("///")
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("deprecated")
            {
                pending_deprecated_doc = true;
            }
            continue;
        }
        previous_line_was_doc = false;

        if let Some(caps) = field_regex.captures(trimmed) {
            let field_name = caps[1].to_string();
            let key = pending_rename.clone().unwrap_or_else(|| field_name.clone());

            if pending_deprecated_attr
                || pending_deprecated_doc
                || field_name.starts_with("deprecated_")
            {
                deprecated.insert(key);
            }

            pending_rename = None;
            pending_deprecated_attr = false;
            pending_deprecated_doc = false;
        }
    }

    Ok(deprecated)
}

fn extract_core_syntax_keys(content: &str) -> Result<BTreeSet<String>> {
    let regex = Regex::new(r"^\|\s*@([a-z0-9_.]+)\s*\|")?;
    let mut in_section = false;
    let mut result = BTreeSet::new();

    for line in content.lines() {
        if line.trim() == "### Syntax highlighting" {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("### ") {
            break;
        }
        if !in_section {
            continue;
        }

        if let Some(caps) = regex.captures(line) {
            result.insert(caps[1].to_string());
        }
    }

    Ok(result)
}

fn extract_observed_syntax_keys(repo_root: &Path) -> Result<BTreeSet<String>> {
    let regex = Regex::new(r"@([A-Za-z0-9_.]+)")?;
    let mut result = BTreeSet::new();

    for entry in WalkDir::new(repo_root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "highlights.scm" {
            continue;
        }

        let content = fs::read_to_string(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        for captures in regex.captures_iter(&strip_string_literals(&content)) {
            let capture = &captures[1];
            // `_`-prefixed captures are tree-sitter helpers that themes never style.
            if capture.starts_with('_') {
                continue;
            }
            result.insert(capture.to_string());
        }
    }

    Ok(result)
}

fn strip_string_literals(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut in_string = false;
    let mut escaped = false;

    for character in content.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }

        if character == '"' {
            in_string = true;
            continue;
        }

        result.push(character);
    }

    result
}

fn expected_syntax_keys(key_sets: &KeySets, level: SyntaxLevel) -> BTreeSet<String> {
    match level {
        SyntaxLevel::Off => BTreeSet::new(),
        SyntaxLevel::Core => key_sets.syntax_core.clone(),
        SyntaxLevel::Observed => key_sets.syntax_observed.clone(),
        SyntaxLevel::Both => key_sets
            .syntax_core
            .union(&key_sets.syntax_observed)
            .cloned()
            .collect(),
    }
}

fn analyze_theme_file(
    repo_root: &Path,
    path: &Path,
    key_sets: &KeySets,
    expected_syntax: &BTreeSet<String>,
    deprecated_mode: DeprecatedMode,
) -> Result<FileReport> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read theme file {}", path.display()))?;

    let value: Value = serde_json_lenient::from_str(&content)
        .with_context(|| format!("failed to parse theme file {}", path.display()))?;

    let themes = value
        .get("themes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            anyhow!(
                "theme family file missing 'themes' array: {}",
                path.display()
            )
        })?;

    let mut theme_reports = Vec::new();

    for (index, theme_value) in themes.iter().enumerate() {
        let theme_name = theme_value
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("theme[{index}]"));

        let style = theme_value
            .get("style")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("theme '{}' has no object 'style'", theme_name))?;

        let mut present_non_syntax = BTreeSet::new();
        let mut syntax_keys = BTreeSet::new();

        for (key, val) in style {
            if key == "accents" || key == "players" {
                continue;
            }
            if key == "syntax" {
                if let Some(map) = val.as_object() {
                    for (capture, capture_style) in map {
                        validate_syntax_style(&theme_name, capture, capture_style)?;
                    }
                    syntax_keys.extend(map.keys().cloned());
                }
                continue;
            }
            validate_non_syntax_value(&theme_name, key, val)?;
            present_non_syntax.insert(key.clone());
        }

        let missing_non_syntax = key_sets
            .expected_non_syntax
            .difference(&present_non_syntax)
            .cloned()
            .collect::<Vec<_>>();
        let unknown_non_syntax = present_non_syntax
            .difference(&key_sets.expected_non_syntax)
            .filter(|key| !key_sets.deprecated_non_syntax.contains(*key))
            .cloned()
            .collect::<Vec<_>>();
        let deprecated_non_syntax = match deprecated_mode {
            DeprecatedMode::Include => present_non_syntax
                .intersection(&key_sets.deprecated_non_syntax)
                .cloned()
                .collect::<Vec<_>>(),
            DeprecatedMode::Exclude => Vec::new(),
        };

        let mut syntax_uncovered = Vec::new();
        let mut syntax_covered_by_ancestor = Vec::new();
        let mut syntax_exact_count = 0usize;

        for capture in expected_syntax {
            if syntax_keys.contains(capture) {
                syntax_exact_count += 1;
                continue;
            }

            if let Some(ancestor) = find_covering_ancestor(capture, &syntax_keys) {
                syntax_covered_by_ancestor.push(AncestorCoverage {
                    capture: capture.clone(),
                    covered_by: ancestor,
                });
                continue;
            }

            syntax_uncovered.push(capture.clone());
        }

        theme_reports.push(ThemeReport {
            theme_name,
            missing_non_syntax,
            unknown_non_syntax,
            deprecated_non_syntax,
            syntax_uncovered,
            syntax_covered_by_ancestor,
            syntax_exact_count,
        });
    }

    let display_path = path
        .strip_prefix(repo_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf());

    Ok(FileReport {
        path: display_path.display().to_string(),
        themes: theme_reports,
    })
}

fn validate_non_syntax_value(theme_name: &str, key: &str, value: &Value) -> Result<()> {
    // Every style field is an `Option`, so `null` means "not set".
    if value.is_null() {
        return Ok(());
    }

    if key == BACKGROUND_APPEARANCE_KEY {
        let appearance = value.as_str().ok_or_else(|| {
            anyhow!("theme '{theme_name}' key '{key}' must be a string, found {value}")
        })?;
        if !BACKGROUND_APPEARANCE_VALUES.contains(&appearance) {
            bail!(
                "theme '{theme_name}' key '{key}' has unsupported value '{appearance}'; expected one of {}",
                BACKGROUND_APPEARANCE_VALUES.join(", ")
            );
        }
        return Ok(());
    }

    let color = value.as_str().ok_or_else(|| {
        anyhow!("theme '{theme_name}' key '{key}' must be a color string, found {value}")
    })?;
    theme::try_parse_color(color)
        .with_context(|| format!("theme '{theme_name}' key '{key}' has invalid color '{color}'"))?;
    Ok(())
}

fn validate_syntax_style(theme_name: &str, capture: &str, value: &Value) -> Result<()> {
    let Some(style) = value.as_object() else {
        return Ok(());
    };
    let Some(color) = style.get("color") else {
        return Ok(());
    };
    if color.is_null() {
        return Ok(());
    }

    let color = color.as_str().ok_or_else(|| {
        anyhow!(
            "theme '{theme_name}' syntax capture '{capture}' color must be a string, found {color}"
        )
    })?;
    theme::try_parse_color(color).with_context(|| {
        format!("theme '{theme_name}' syntax capture '{capture}' has invalid color '{color}'")
    })?;
    Ok(())
}

fn find_covering_ancestor(capture: &str, syntax_keys: &BTreeSet<String>) -> Option<String> {
    let mut parts: Vec<&str> = capture.split('.').collect();
    while parts.len() > 1 {
        parts.pop();
        let candidate = parts.join(".");
        if syntax_keys.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn summarize(files: Vec<FileReport>) -> SummaryReport {
    let mut total_missing_non_syntax = 0;
    let mut total_unknown_non_syntax = 0;
    let mut total_deprecated_non_syntax = 0;
    let mut total_syntax_uncovered = 0;
    let mut total_syntax_covered_by_ancestor = 0;

    for file in &files {
        for theme in &file.themes {
            total_missing_non_syntax += theme.missing_non_syntax.len();
            total_unknown_non_syntax += theme.unknown_non_syntax.len();
            total_deprecated_non_syntax += theme.deprecated_non_syntax.len();
            total_syntax_uncovered += theme.syntax_uncovered.len();
            total_syntax_covered_by_ancestor += theme.syntax_covered_by_ancestor.len();
        }
    }

    SummaryReport {
        files,
        total_missing_non_syntax,
        total_unknown_non_syntax,
        total_deprecated_non_syntax,
        total_syntax_uncovered,
        total_syntax_covered_by_ancestor,
    }
}

fn format_counts(counts: &[(usize, &str)]) -> String {
    let parts = counts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect::<Vec<_>>();

    if parts.is_empty() {
        "ok".to_string()
    } else {
        parts.join(", ")
    }
}

fn print_text_report(summary: &SummaryReport) {
    let name_width = summary
        .files
        .iter()
        .flat_map(|file| file.themes.iter())
        .map(|theme| theme.theme_name.chars().count())
        .max()
        .unwrap_or(0);

    for (file_index, file) in summary.files.iter().enumerate() {
        if file_index > 0 {
            println!();
        }
        println!("{}", file.path);

        for theme in &file.themes {
            let counts = format_counts(&[
                (theme.syntax_exact_count, "exact"),
                (theme.syntax_covered_by_ancestor.len(), "inherited"),
                (theme.missing_non_syntax.len(), "missing"),
                (theme.unknown_non_syntax.len(), "unknown"),
                (theme.deprecated_non_syntax.len(), "deprecated"),
                (theme.syntax_uncovered.len(), "uncovered"),
            ]);
            println!("  {:<name_width$}  {counts}", theme.theme_name);

            for key in &theme.missing_non_syntax {
                println!("    {:<10}  {key}", "missing");
            }
            for key in &theme.unknown_non_syntax {
                println!("    {:<10}  {key}", "unknown");
            }
            for key in &theme.deprecated_non_syntax {
                println!("    {:<10}  {key}", "deprecated");
            }
            for key in &theme.syntax_uncovered {
                println!("    {:<10}  {key}", "uncovered");
            }
            for coverage in &theme.syntax_covered_by_ancestor {
                println!(
                    "    {:<10}  {} <- {}",
                    "inherited", coverage.capture, coverage.covered_by
                );
            }
        }
    }

    println!();
    println!(
        "totals: {}",
        format_counts(&[
            (summary.total_missing_non_syntax, "missing"),
            (summary.total_unknown_non_syntax, "unknown"),
            (summary.total_deprecated_non_syntax, "deprecated"),
            (summary.total_syntax_uncovered, "uncovered"),
            (summary.total_syntax_covered_by_ancestor, "inherited"),
        ])
    );
}

// Annotations go to stderr so `--format json` stays parseable on stdout.
fn print_github_annotations(summary: &SummaryReport) {
    for file in &summary.files {
        for theme in &file.themes {
            for key in &theme.missing_non_syntax {
                eprintln!(
                    "::error file={}::theme '{}' missing non-syntax key '{}'",
                    file.path, theme.theme_name, key
                );
            }
            for key in &theme.unknown_non_syntax {
                eprintln!(
                    "::error file={}::theme '{}' unknown non-syntax key '{}'",
                    file.path, theme.theme_name, key
                );
            }
            for key in &theme.deprecated_non_syntax {
                eprintln!(
                    "::warning file={}::theme '{}' deprecated non-syntax key '{}'",
                    file.path, theme.theme_name, key
                );
            }
            for key in &theme.syntax_uncovered {
                eprintln!(
                    "::error file={}::theme '{}' uncovered syntax capture '{}'",
                    file.path, theme.theme_name, key
                );
            }
            for coverage in &theme.syntax_covered_by_ancestor {
                eprintln!(
                    "::warning file={}::theme '{}' syntax capture '{}' covered by ancestor '{}'",
                    file.path, theme.theme_name, coverage.capture, coverage.covered_by
                );
            }
        }
    }
}

fn parse_fail_kinds(value: &str) -> Result<BTreeSet<FailKind>> {
    let mut kinds = BTreeSet::new();
    for raw in value.split(',') {
        let token = raw.trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }

        let kind = match token.as_str() {
            "uncovered" => FailKind::Uncovered,
            "unknown" => FailKind::Unknown,
            "deprecated" => FailKind::Deprecated,
            _ => bail!("unsupported --fail-on kind: {token}"),
        };
        kinds.insert(kind);
    }
    Ok(kinds)
}

fn has_policy_violations(summary: &SummaryReport, fail_kinds: &BTreeSet<FailKind>) -> bool {
    fail_kinds.iter().any(|kind| match kind {
        FailKind::Uncovered => {
            summary.total_missing_non_syntax + summary.total_syntax_uncovered > 0
        }
        FailKind::Unknown => summary.total_unknown_non_syntax > 0,
        FailKind::Deprecated => summary.total_deprecated_non_syntax > 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_covering_prefers_closest() {
        let set = BTreeSet::from([
            "function".to_string(),
            "function.method".to_string(),
            "variable".to_string(),
        ]);

        assert_eq!(
            find_covering_ancestor("function.method.call", &set),
            Some("function.method".to_string())
        );
        assert_eq!(
            find_covering_ancestor("variable.local", &set),
            Some("variable".to_string())
        );
        assert_eq!(find_covering_ancestor("tag.doctype", &set), None);
    }

    #[test]
    fn parses_fail_kinds() {
        let kinds = parse_fail_kinds("unknown, uncovered,deprecated").unwrap();
        assert!(kinds.contains(&FailKind::Unknown));
        assert!(kinds.contains(&FailKind::Uncovered));
        assert!(kinds.contains(&FailKind::Deprecated));
    }

    #[test]
    fn extracts_core_syntax_keys_from_docs_table() {
        let input = "### Syntax highlighting\n| @comment | Captures comments |\n| @function.method | x |\n### Bracket matching\n| @open | x |";
        let result = extract_core_syntax_keys(input).unwrap();
        assert!(result.contains("comment"));
        assert!(result.contains("function.method"));
        assert!(!result.contains("open"));
    }

    #[test]
    fn extracts_serde_renames_from_struct_block() {
        let input = "pub struct Demo {\n    #[serde(rename = \"border.variant\")]\n    pub border_variant: Option<ThemeColor>,\n    #[serde(rename = \"scrollbar_thumb.background\", skip_serializing)]\n    pub scrollbar_thumb: Option<ThemeColor>,\n    #[serde(default)]\n    pub plain: Option<ThemeColor>,\n}";
        let renames = extract_serde_renames(input).unwrap();

        assert_eq!(
            renames,
            BTreeSet::from([
                "border.variant".to_string(),
                "scrollbar_thumb.background".to_string(),
            ])
        );
    }

    #[test]
    fn marks_only_fields_that_are_deprecated() {
        let input = "\
/// Deprecated in favor of `new_name`.\n\
#[deprecated]\n\
pub old_name: Option<ThemeColor>,\n\
\n\
/// Replaces the deprecated foo color.\n\
#[serde(rename = \"live.key\")]\n\
pub live: Option<ThemeColor>,\n\
\n\
/// The deprecated version of `scrollbar.thumb.background`.\n\
#[serde(rename = \"scrollbar_thumb.background\")]\n\
pub deprecated_scrollbar_thumb_background: Option<ThemeColor>,\n\
\n\
/// A normal field.\n\
#[serde(rename = \"normal.key\")]\n\
pub normal: Option<ThemeColor>,";

        let deprecated = extract_deprecated_keys(input).unwrap();

        assert_eq!(
            deprecated,
            BTreeSet::from([
                "old_name".to_string(),
                "scrollbar_thumb.background".to_string(),
            ])
        );
    }

    #[test]
    fn ignores_string_literals_and_internal_captures() {
        let input = "(identifier) @function\n(#any-of? @function \"@media\" \"@charset\")\n(identifier) @_parent";
        let regex = Regex::new(r"@([A-Za-z0-9_.]+)").unwrap();
        let captures = regex
            .captures_iter(&strip_string_literals(input))
            .map(|caps| caps[1].to_string())
            .filter(|capture| !capture.starts_with('_'))
            .collect::<BTreeSet<_>>();

        assert_eq!(captures, BTreeSet::from(["function".to_string()]));
    }

    #[test]
    fn rejects_invalid_non_syntax_values() {
        assert!(validate_non_syntax_value("t", "background", &Value::from("#ffffff")).is_ok());
        assert!(validate_non_syntax_value("t", "background", &Value::from("not-a-color")).is_err());
        assert!(validate_non_syntax_value("t", "text", &Value::from(123)).is_err());
        assert!(validate_non_syntax_value("t", "text", &Value::Null).is_ok());
        assert!(
            validate_non_syntax_value("t", BACKGROUND_APPEARANCE_KEY, &Value::from("transparent"))
                .is_ok()
        );
        assert!(
            validate_non_syntax_value("t", BACKGROUND_APPEARANCE_KEY, &Value::from("glowing"))
                .is_err()
        );
    }

    fn summary_with_totals(
        missing: usize,
        unknown: usize,
        deprecated: usize,
        uncovered: usize,
    ) -> SummaryReport {
        SummaryReport {
            files: Vec::new(),
            total_missing_non_syntax: missing,
            total_unknown_non_syntax: unknown,
            total_deprecated_non_syntax: deprecated,
            total_syntax_uncovered: uncovered,
            total_syntax_covered_by_ancestor: 0,
        }
    }

    #[test]
    fn formats_counts_without_zero_categories() {
        assert_eq!(
            format_counts(&[(37, "exact"), (5, "inherited"), (0, "missing")]),
            "37 exact, 5 inherited"
        );
        assert_eq!(format_counts(&[(0, "missing"), (0, "unknown")]), "ok");
    }

    #[test]
    fn fails_policy_only_for_configured_kinds() {
        let deprecated_only = BTreeSet::from([FailKind::Deprecated]);
        let unknown_only = BTreeSet::from([FailKind::Unknown]);

        assert!(has_policy_violations(
            &summary_with_totals(0, 0, 2, 0),
            &deprecated_only
        ));
        assert!(!has_policy_violations(
            &summary_with_totals(0, 0, 2, 0),
            &unknown_only
        ));
        assert!(!has_policy_violations(
            &summary_with_totals(0, 0, 0, 0),
            &deprecated_only
        ));
    }

    #[test]
    fn reports_missing_unknown_and_deprecated_keys() {
        let path = std::env::temp_dir().join(format!(
            "theme_checker_test_{}_{}.json",
            std::process::id(),
            line!()
        ));
        fs::write(
            &path,
            r##"{"themes":[{"name":"Demo","style":{"background":"#000000","bogus.key":"#ffffff","scrollbar_thumb.background":"#111111","syntax":{"function":"#ffffff"}}}]}"##,
        )
        .unwrap();

        let key_sets = KeySets {
            expected_non_syntax: BTreeSet::from(["background".to_string(), "text".to_string()]),
            deprecated_non_syntax: BTreeSet::from(["scrollbar_thumb.background".to_string()]),
            syntax_core: BTreeSet::from(["function".to_string(), "keyword".to_string()]),
            syntax_observed: BTreeSet::new(),
        };
        let expected_syntax = BTreeSet::from(["function".to_string(), "keyword".to_string()]);

        let report = analyze_theme_file(
            Path::new("/"),
            &path,
            &key_sets,
            &expected_syntax,
            DeprecatedMode::Include,
        )
        .unwrap();
        fs::remove_file(&path).unwrap();

        let theme = &report.themes[0];
        assert_eq!(theme.missing_non_syntax, vec!["text".to_string()]);
        assert_eq!(theme.unknown_non_syntax, vec!["bogus.key".to_string()]);
        assert_eq!(
            theme.deprecated_non_syntax,
            vec!["scrollbar_thumb.background".to_string()]
        );
        assert_eq!(theme.syntax_uncovered, vec!["keyword".to_string()]);
        assert_eq!(theme.syntax_exact_count, 1);
    }
}
