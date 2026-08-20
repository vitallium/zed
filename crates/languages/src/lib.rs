use futures::FutureExt as _;
use gpui::{App, SharedString, UpdateGlobal};
use node_runtime::NodeRuntime;
use project::Fs;
use python::PyprojectTomlManifestProvider;
use rust::CargoManifestProvider;
use settings::{SemanticTokenRules, SettingsStore};
use smol::stream::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use util::ResultExt;

pub use language::*;

use crate::{
    json::JsonTaskProvider,
    python::{BasedPyrightLspAdapter, RuffLspAdapter},
};

fn log_lang_init(label: &str, start: &Instant) {
    use std::io::Write;
    let elapsed = start.elapsed();
    eprintln!("LANG_INIT: {}: {:?}", label, elapsed);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/zed_lang_init_times.log")
    {
        let _ = writeln!(file, "{}: {:?}", label, elapsed);
    }
}

mod bash;
mod c;
mod cpp;
mod css;
mod eslint;
mod go;
mod json;
mod package_json;
mod python;
mod rust;
mod tailwind;
mod tailwindcss;
mod typescript;
mod vtsls;
mod yaml;

pub(crate) use package_json::{PackageJson, PackageJsonData};

/// A shared grammar for plain text, exposed for reuse by downstream crates.
#[cfg(feature = "tree-sitter-gitcommit")]
pub static LANGUAGE_GIT_COMMIT: std::sync::LazyLock<Arc<Language>> =
    std::sync::LazyLock::new(|| {
        Arc::new(Language::new(
            LanguageConfig {
                name: "Git Commit".into(),
                soft_wrap: Some(language::SoftWrap::EditorWidth),
                matcher: (LanguageMatcher {
                    path_suffixes: vec!["COMMIT_EDITMSG".to_owned()],
                    first_line_pattern: None,
                    ..LanguageMatcher::default()
                })
                .into(),
                line_comments: vec![Arc::from("#")],
                ..LanguageConfig::default()
            },
            Some(tree_sitter_gitcommit::LANGUAGE.into()),
        ))
    });

pub fn init(languages: Arc<LanguageRegistry>, fs: Arc<dyn Fs>, node: NodeRuntime, cx: &mut App) {
    let start = Instant::now();
    eprintln!("LANG_INIT: start");
    
    #[cfg(feature = "load-grammars")]
    {
        let native_start = Instant::now();
        languages.register_native_grammars(grammars::native_grammars());
        log_lang_init("native_grammars_registered", &start);
    }

    let bash_lsp_adapter = Arc::new(bash::BashLspAdapter::new(node.clone()));
    log_lang_init("bash_adapter_created", &start);
    let c_lsp_adapter = Arc::new(c::CLspAdapter);
    let css_lsp_adapter = Arc::new(css::CssLspAdapter::new(node.clone()));
    let eslint_adapter = Arc::new(eslint::EsLintLspAdapter::new(node.clone(), fs.clone()));
    let go_context_provider = Arc::new(go::GoContextProvider);
    let go_lsp_adapter = Arc::new(go::GoLspAdapter);
    let json_context_provider = Arc::new(JsonTaskProvider);
    let json_lsp_adapter = Arc::new(json::JsonLspAdapter::new(languages.clone(), node.clone()));
    let node_version_lsp_adapter = Arc::new(json::NodeVersionAdapter);
    let py_lsp_adapter = Arc::new(python::PyLspAdapter::new());
    let ty_lsp_adapter = Arc::new(python::TyLspAdapter::new(fs.clone()));
    let python_context_provider = Arc::new(python::PythonContextProvider);
    let python_lsp_adapter = Arc::new(python::PyrightLspAdapter::new(node.clone()));
    let basedpyright_lsp_adapter = Arc::new(BasedPyrightLspAdapter::new(node.clone()));
    let ruff_lsp_adapter = Arc::new(RuffLspAdapter::new(fs.clone()));
    let python_toolchain_provider = Arc::new(python::PythonToolchainProvider::new(fs.clone()));
    let rust_context_provider = Arc::new(rust::RustContextProvider);
    let rust_lsp_adapter = Arc::new(rust::RustLspAdapter);
    let tailwind_adapter = Arc::new(tailwind::TailwindLspAdapter::new(node.clone()));
    let tailwindcss_adapter = Arc::new(tailwindcss::TailwindCssLspAdapter::new(node.clone()));
    let typescript_context = Arc::new(typescript::TypeScriptContextProvider::new(fs.clone()));
    let typescript_lsp_adapter = Arc::new(typescript::TypeScriptLspAdapter::new(
        node.clone(),
        fs.clone(),
    ));
    let vtsls_adapter = Arc::new(vtsls::VtslsLspAdapter::new(node.clone(), fs.clone()));
    let yaml_lsp_adapter = Arc::new(yaml::YamlLspAdapter::new(node));
    log_lang_init("all_adapters_created", &start);

    let built_in_languages = [
        LanguageInfo {
            name: "bash",
            context: Some(Arc::new(bash::bash_task_context())),
            adapters: vec![bash_lsp_adapter],
            ..Default::default()
        },
        LanguageInfo {
            name: "c",
            adapters: vec![c_lsp_adapter.clone()],
            ..Default::default()
        },
        LanguageInfo {
            name: "cpp",
            adapters: vec![c_lsp_adapter],
            semantic_token_rules: Some(cpp::semantic_token_rules()),
            ..Default::default()
        },
        LanguageInfo {
            name: "css",
            adapters: vec![css_lsp_adapter],
            ..Default::default()
        },
        LanguageInfo {
            name: "diff",
            adapters: vec![],
            ..Default::default()
        },
        LanguageInfo {
            name: "go",
            adapters: vec![go_lsp_adapter.clone()],
            context: Some(go_context_provider.clone()),
            semantic_token_rules: Some(go::semantic_token_rules()),
            ..Default::default()
        },
        LanguageInfo {
            name: "gomod",
            adapters: vec![go_lsp_adapter.clone()],
            context: Some(go_context_provider.clone()),
            ..Default::default()
        },
        LanguageInfo {
            name: "gowork",
            adapters: vec![go_lsp_adapter],
            context: Some(go_context_provider),
            ..Default::default()
        },
        LanguageInfo {
            name: "json",
            adapters: vec![json_lsp_adapter.clone(), node_version_lsp_adapter],
            context: Some(json_context_provider.clone()),
            ..Default::default()
        },
        LanguageInfo {
            name: "jsonc",
            adapters: vec![json_lsp_adapter],
            context: Some(json_context_provider),
            ..Default::default()
        },
        LanguageInfo {
            name: "markdown",
            adapters: vec![],
            ..Default::default()
        },
        LanguageInfo {
            name: "markdown-inline",
            adapters: vec![],
            ..Default::default()
        },
        LanguageInfo {
            name: "python",
            adapters: vec![
                basedpyright_lsp_adapter,
                ruff_lsp_adapter,
                ty_lsp_adapter,
                py_lsp_adapter,
                python_lsp_adapter,
            ],
            context: Some(python_context_provider),
            toolchain: Some(python_toolchain_provider),
            manifest_name: Some(SharedString::new_static("pyproject.toml").into()),
            semantic_token_rules: Some(python::semantic_token_rules()),
        },
        LanguageInfo {
            name: "rust",
            adapters: vec![rust_lsp_adapter],
            context: Some(rust_context_provider),
            manifest_name: Some(SharedString::new_static("Cargo.toml").into()),
            semantic_token_rules: Some(rust::semantic_token_rules()),
            ..Default::default()
        },
        LanguageInfo {
            name: "tsx",
            adapters: vec![typescript_lsp_adapter.clone(), vtsls_adapter.clone()],
            context: Some(typescript_context.clone()),
            ..Default::default()
        },
        LanguageInfo {
            name: "typescript",
            adapters: vec![typescript_lsp_adapter.clone(), vtsls_adapter.clone()],
            context: Some(typescript_context.clone()),
            ..Default::default()
        },
        LanguageInfo {
            name: "javascript",
            adapters: vec![typescript_lsp_adapter.clone(), vtsls_adapter.clone()],
            context: Some(typescript_context),
            ..Default::default()
        },
        LanguageInfo {
            name: "jsdoc",
            adapters: vec![typescript_lsp_adapter.clone(), vtsls_adapter.clone()],
            ..Default::default()
        },
        LanguageInfo {
            name: "regex",
            adapters: vec![],
            ..Default::default()
        },
        LanguageInfo {
            name: "yaml",
            adapters: vec![yaml_lsp_adapter],
            ..Default::default()
        },
        LanguageInfo {
            name: "gitcommit",
            ..Default::default()
        },
        LanguageInfo {
            name: "zed-keybind-context",
            ..Default::default()
        },
    ];

    log_lang_init("starting_language_registration", &start);
    
    // Collect semantic token rules to batch update them at the end
    // This avoids calling recompute_values multiple times
    let mut semantic_token_rules_to_set = Vec::new();
    
    for registration in built_in_languages {
        let config = load_config(registration.name);
        let name = registration.name;
        let context = registration.context.clone();
        let toolchain = registration.toolchain.clone();
        let manifest_name = registration.manifest_name.clone();
        let config_for_closure = config.clone();
        
        for adapter in registration.adapters {
            languages.register_lsp_adapter(config.name.clone(), adapter);
        }
        languages.register_language(
            config.name.clone(),
            config.grammar.clone(),
            config.matcher.clone(),
            config.hidden,
            manifest_name.clone(),
            Arc::new(move || {
                let config = config_for_closure.clone();
                let context = context.clone();
                let toolchain = toolchain.clone();
                let manifest_name = manifest_name.clone();
                async move {
                    Ok(LoadedLanguage {
                        config,
                        queries: grammars::load_queries(name),
                        context_provider: context,
                        toolchain_provider: toolchain,
                        manifest_name,
                    })
                }
                .boxed()
            }),
        );
        
        // Collect semantic token rules for batch update
        if let Some(rules) = registration.semantic_token_rules {
            semantic_token_rules_to_set.push((config.name.0.clone(), rules));
        }
    }
    
    // Batch update all semantic token rules at once
    if !semantic_token_rules_to_set.is_empty() {
        SettingsStore::update_global(cx, |store, cx| {
            store.set_language_semantic_token_rules_batch(semantic_token_rules_to_set, cx);
        });
    }
    
    log_lang_init("all_languages_registered", &start);

    // Register globally available language servers.
    //
    // This will allow users to add support for a built-in language server (e.g., Tailwind)
    // for a given language via the `language_servers` setting:
    //
    // ```json
    // {
    //   "languages": {
    //     "My Language": {
    //       "language_servers": ["tailwindcss-language-server", "..."]
    //     }
    //   }
    // }
    // ```
    languages.register_available_lsp_adapter(
        LanguageServerName("tailwindcss-language-server".into()),
        tailwind_adapter.clone(),
    );
    languages.register_available_lsp_adapter(
        LanguageServerName("tailwindcss-intellisense-css".into()),
        tailwindcss_adapter,
    );
    languages.register_available_lsp_adapter(
        LanguageServerName("eslint".into()),
        eslint_adapter.clone(),
    );
    languages.register_available_lsp_adapter(LanguageServerName("vtsls".into()), vtsls_adapter);
    languages.register_available_lsp_adapter(
        LanguageServerName("typescript-language-server".into()),
        typescript_lsp_adapter,
    );

    // Register Tailwind for the existing languages that should have it by default.
    //
    // This can be driven by the `language_servers` setting once we have a way for
    // extensions to provide their own default value for that setting.
    let tailwind_languages = [
        "Astro",
        "CSS",
        "ERB",
        "HTML+ERB",
        "HEEx",
        "HTML",
        "JavaScript",
        "TypeScript",
        "PHP",
        "Svelte",
        "TSX",
        "Vue.js",
    ];

    for language in tailwind_languages {
        languages.register_lsp_adapter(language.into(), tailwind_adapter.clone());
    }
    log_lang_init("tailwind_registered", &start);

    let eslint_languages = ["TSX", "TypeScript", "JavaScript", "Vue.js", "Svelte"];
    for language in eslint_languages {
        languages.register_lsp_adapter(language.into(), eslint_adapter.clone());
    }
    log_lang_init("eslint_registered", &start);

    let mut subscription = languages.subscribe();
    log_lang_init("subscription_created", &start);
    let mut prev_language_settings = languages.language_settings();

    cx.spawn(async move |cx| {
        while subscription.next().await.is_some() {
            let language_settings = languages.language_settings();
            if language_settings != prev_language_settings {
                cx.update(|cx| {
                    SettingsStore::update_global(cx, |settings, cx| {
                        settings
                            .set_extension_settings(
                                settings::ExtensionsSettingsContent {
                                    all_languages: language_settings.clone(),
                                },
                                cx,
                            )
                            .log_err();
                    });
                });
                prev_language_settings = language_settings;
            }
        }
        anyhow::Ok(())
    })
    .detach();
    let manifest_providers: [Arc<dyn ManifestProvider>; 2] = [
        Arc::from(CargoManifestProvider),
        Arc::from(PyprojectTomlManifestProvider),
    ];
    for provider in manifest_providers {
        project::ManifestProvidersStore::global(cx).register(provider);
    }
    log_lang_init("languages_init_complete", &start);
}

#[derive(Default)]
struct LanguageInfo {
    name: &'static str,
    adapters: Vec<Arc<dyn LspAdapter>>,
    context: Option<Arc<dyn ContextProvider>>,
    toolchain: Option<Arc<dyn ToolchainLister>>,
    manifest_name: Option<ManifestName>,
    semantic_token_rules: Option<SemanticTokenRules>,
}

#[cfg(any(test, feature = "test-support"))]
pub fn language(name: &str, grammar: tree_sitter::Language) -> Arc<Language> {
    Arc::new(
        Language::new(grammars::load_config(name), Some(grammar))
            .with_queries(grammars::load_queries(name))
            .unwrap(),
    )
}

fn load_config(name: &str) -> LanguageConfig {
    let grammars_loaded = cfg!(any(feature = "load-grammars", test));
    grammars::load_config_for_feature(name, grammars_loaded)
}
