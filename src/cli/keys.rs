//! CLI: `donsetch keys <subcommand>`
//!
//!   donsetch keys add <provider> <key>    Add a key (auto-sets default if first)
//!   donsetch keys remove <provider> [key] Remove a key (or all if no key)
//!   donsetch keys list                    Show all providers and key states
//!   donsetch keys default <provider>      Set the default/first provider
//!   donsetch keys reset [provider]        Reset key states to active
//!
//! Providers: tavily, exa, serper, tinyfish

use super::{bold, dim, green, red};

use crate::search::byok::store::{ByokConfig, PROVIDERS, render_list};

pub fn run(args: &[String]) {
    let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");

    match sub {
        "add" => cmd_add(args),
        "remove" | "rm" => cmd_remove(args),
        "list" | "ls" => cmd_list(),
        "default" => cmd_default(args),
        "reset" => cmd_reset(args),
        "help" | "-h" | "--help" => print_help(),
        _ => {
            eprintln!("{} unknown subcommand: {sub}", red("\u{2717}"));
            print_help();
            std::process::exit(1);
        }
    }
}

fn cmd_add(args: &[String]) {
    let provider = match args.get(3) {
        Some(p) => p,
        None => {
            eprintln!(
                "{} usage: donsetch keys add <provider> <key>",
                red("\u{2717}")
            );
            eprintln!("   providers: {}", dim(&PROVIDERS.join(", ")));
            std::process::exit(1);
        }
    };
    let key = match args.get(4) {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => {
            eprintln!(
                "{} usage: donsetch keys add <provider> <key>",
                red("\u{2717}")
            );
            std::process::exit(1);
        }
    };

    let provider = provider.to_lowercase();

    if !PROVIDERS.contains(&provider.as_str()) {
        eprintln!(
            "{} unknown provider: {provider}\n   providers: {}",
            red("\u{2717}"),
            dim(&PROVIDERS.join(", "))
        );
        std::process::exit(1);
    }

    let mut cfg = ByokConfig::load();
    let is_new = !cfg.providers.iter().any(|p| p.name == *provider);
    let was_first = cfg.providers.is_empty();

    cfg.add_key(&provider, &key);
    cfg.save();

    if was_first {
        println!(
            "  {} added key to {} (set as default)",
            green("\u{2713}"),
            bold(&provider)
        );
    } else if is_new {
        println!(
            "  {} added key to {} (stacked — {} providers now configured)",
            green("\u{2713}"),
            bold(&provider),
            cfg.providers.len()
        );
    } else {
        println!(
            "  {} stacked key onto {} ({} keys total)",
            green("\u{2713}"),
            bold(&provider),
            cfg.providers
                .iter()
                .find(|p| p.name == *provider)
                .map(|p| p.keys.len())
                .unwrap_or(0)
        );
    }

    if cfg.providers.len() == 1 {
        println!();
        println!(
            "  {} BYOK search is now active — local search is bypassed.",
            dim("note:")
        );
        println!("  {} Restart your MCP server if running.", dim("      "));
    }
}

fn cmd_remove(args: &[String]) {
    let provider = match args.get(3) {
        Some(p) => p.to_lowercase(),
        None => {
            eprintln!(
                "{} usage: donsetch keys remove <provider> [key]",
                red("\u{2717}")
            );
            eprintln!("   providers: {}", dim(&PROVIDERS.join(", ")));
            std::process::exit(1);
        }
    };

    let mut cfg = ByokConfig::load();
    let key = args.get(4).map(|s| s.trim());

    let removed = cfg.remove_keys(&provider, key);
    if !removed {
        eprintln!("  {} no matching key found for {provider}", red("\u{2717}"));
        std::process::exit(1);
    }

    cfg.save();

    let remaining = cfg
        .providers
        .iter()
        .find(|p| p.name == *provider)
        .map(|p| p.keys.len())
        .unwrap_or(0);

    if remaining == 0 {
        if cfg.is_configured() {
            println!(
                "  {} removed all keys for {} (provider removed, default={})",
                green("\u{2713}"),
                bold(&provider),
                green(&cfg.default)
            );
        } else {
            println!(
                "  {} removed all keys for {} — no providers remaining",
                green("\u{2713}"),
                bold(&provider)
            );
            println!(
                "  {} BYOK search disabled — local search is active.",
                dim("note:")
            );
        }
    } else {
        println!(
            "  {} removed key from {} ({} keys remaining)",
            green("\u{2713}"),
            bold(&provider),
            remaining
        );
    }
}

fn cmd_list() {
    let cfg = ByokConfig::load();
    render_list(&cfg);
}

fn cmd_default(args: &[String]) {
    let provider = match args.get(3) {
        Some(p) => p.to_lowercase(),
        None => {
            eprintln!(
                "{} usage: donsetch keys default <provider>",
                red("\u{2717}")
            );
            eprintln!("   providers: {}", dim(&PROVIDERS.join(", ")));
            std::process::exit(1);
        }
    };

    let mut cfg = ByokConfig::load();
    if !cfg.is_configured() {
        eprintln!("  {} no keys configured", red("\u{2717}"));
        std::process::exit(1);
    }

    if cfg.set_default(&provider) {
        cfg.save();
        println!(
            "  {} default provider set to {}",
            green("\u{2713}"),
            bold(&provider)
        );
    } else {
        eprintln!(
            "  {} provider {provider} has no keys configured",
            red("\u{2717}")
        );
        std::process::exit(1);
    }
}

fn cmd_reset(args: &[String]) {
    let provider = args.get(3).map(|s| s.to_lowercase());

    let mut cfg = ByokConfig::load();
    if !cfg.is_configured() {
        eprintln!("  {} no keys configured", red("\u{2717}"));
        std::process::exit(1);
    }

    // Check provider has keys if specified.
    if let Some(p) = &provider
        && !cfg.providers.iter().any(|pc| &pc.name == p)
    {
        eprintln!("  {} provider {p} has no keys configured", red("\u{2717}"));
        std::process::exit(1);
    }

    cfg.reset_states(provider.as_deref());
    cfg.save();

    match &provider {
        Some(p) => println!(
            "  {} reset all key states for {} to active",
            green("\u{2713}"),
            bold(p)
        ),
        None => println!(
            "  {} reset all key states to active ({} providers)",
            green("\u{2713}"),
            cfg.providers.len()
        ),
    }
}

fn print_help() {
    println!(
        "{}",
        bold("donsetch keys — BYOK search provider management")
    );
    println!();
    println!("  {}", bold("Commands:"));
    println!(
        "    {} <provider> <key>    Add a key (auto-sets default if first)",
        green("add")
    );
    println!(
        "    {} <provider> [key]   Remove a key (or all keys for a provider)",
        green("remove")
    );
    println!(
        "    {}                     Show all providers, keys, and states",
        green("list")
    );
    println!(
        "    {} <provider>          Set the default/first-priority provider",
        green("default")
    );
    println!(
        "    {} [provider]         Reset key states to active (fixes rate-limited/dead keys)",
        green("reset")
    );
    println!();
    println!("  {}", bold("Providers:"));
    println!("    tavily    Tavily Search API (api.tavily.com)");
    println!("    exa       Exa AI Search (api.exa.ai)");
    println!("    serper    Serper.dev Google SERP (google.serper.dev)");
    println!("    tinyfish  TinyFish Search (api.search.tinyfish.ai)");
    println!();
    println!("  {}", bold("Stacking:"));
    println!("    Add multiple keys to the same provider for rotation.");
    println!("    If one key hits a rate limit or runs out of credits,");
    println!("    the next key is tried automatically.");
    println!();
    println!("  {}", bold("Fallback:"));
    println!("    If all providers are exhausted, DonSeTch falls back");
    println!("    to the local keyless 5-engine search system.");
}
