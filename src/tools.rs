pub struct ToolSpec {
    pub display: &'static str,
    pub icon: Option<&'static str>,
    needles: &'static [&'static str],
}

pub static TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        display: "Storybook",
        icon: Some(""),
        needles: &["storybook"],
    },
    ToolSpec {
        display: "Next.js",
        icon: Some(""),
        needles: &["next.js", "next"],
    },
    ToolSpec {
        display: "Astro",
        icon: Some(""),
        needles: &["astro"],
    },
    ToolSpec {
        display: "Nuxt",
        icon: Some(""),
        needles: &["nuxt"],
    },
    ToolSpec {
        display: "Vite",
        icon: Some(""),
        needles: &["vite"],
    },
    ToolSpec {
        display: "Vue",
        icon: Some(""),
        needles: &["vue"],
    },
    ToolSpec {
        display: "Svelte",
        icon: Some(""),
        needles: &["svelte"],
    },
    ToolSpec {
        display: "Angular",
        icon: Some(""),
        needles: &["angular"],
    },
    ToolSpec {
        display: "React",
        icon: Some(""),
        needles: &["react"],
    },
    ToolSpec {
        display: "Webpack",
        icon: Some(""),
        needles: &["webpack"],
    },
    ToolSpec {
        display: "Parcel",
        icon: None,
        needles: &["parcel"],
    },
    ToolSpec {
        display: "Express",
        icon: Some(""),
        needles: &["express"],
    },
    ToolSpec {
        display: "Fastify",
        icon: Some(""),
        needles: &["fastify"],
    },
    ToolSpec {
        display: "NestJS",
        icon: Some(""),
        needles: &["nestjs", "nest"],
    },
    ToolSpec {
        display: "Django",
        icon: Some(""),
        needles: &["django"],
    },
    ToolSpec {
        display: "FastAPI",
        icon: Some(""),
        needles: &["fastapi"],
    },
    ToolSpec {
        display: "Flask",
        icon: Some(""),
        needles: &["flask"],
    },
    ToolSpec {
        display: "Uvicorn",
        icon: None,
        needles: &["uvicorn"],
    },
    ToolSpec {
        display: "Gunicorn",
        icon: None,
        needles: &["gunicorn"],
    },
    ToolSpec {
        display: "Rails",
        icon: Some(""),
        needles: &["rails"],
    },
    ToolSpec {
        display: "Spring Boot",
        icon: Some(""),
        needles: &["spring boot", "spring"],
    },
    ToolSpec {
        display: "Laravel",
        icon: Some(""),
        needles: &["laravel", "artisan"],
    },
    ToolSpec {
        display: "Nginx",
        icon: Some(""),
        needles: &["nginx"],
    },
    ToolSpec {
        display: "Caddy",
        icon: None,
        needles: &["caddy"],
    },
    ToolSpec {
        display: "Apache",
        icon: Some(""),
        needles: &["apache"],
    },
    ToolSpec {
        display: "Bun",
        icon: Some(""),
        needles: &["bun"],
    },
    ToolSpec {
        display: "Deno",
        icon: Some(""),
        needles: &["deno"],
    },
    ToolSpec {
        display: "Node",
        icon: Some(""),
        needles: &["node"],
    },
    ToolSpec {
        display: "Python",
        icon: Some(""),
        needles: &["python", "python3"],
    },
    ToolSpec {
        display: "Ruby",
        icon: Some(""),
        needles: &["ruby"],
    },
    ToolSpec {
        display: "Rust",
        icon: Some(""),
        needles: &["cargo", "rust"],
    },
    ToolSpec {
        display: "Go",
        icon: Some(""),
        needles: &["golang", "go"],
    },
    ToolSpec {
        display: "Java",
        icon: Some(""),
        needles: &["java"],
    },
    ToolSpec {
        display: "PHP",
        icon: Some(""),
        needles: &["php"],
    },
    ToolSpec {
        display: "Docker",
        icon: Some(""),
        needles: &["docker"],
    },
];

pub fn tool_for_text(text: &str) -> Option<&'static ToolSpec> {
    let normalized = text.to_ascii_lowercase();
    best_tool(&[normalized])
}

pub fn tool_for_labels(labels: &[String]) -> Option<&'static ToolSpec> {
    best_tool(labels)
}

fn best_tool(labels: &[String]) -> Option<&'static ToolSpec> {
    best_scored_tool(labels, false).or_else(|| best_scored_tool(labels, true))
}

fn best_scored_tool(labels: &[String], generic_runtime: bool) -> Option<&'static ToolSpec> {
    let mut best = None;
    let mut best_score = 0;
    for tool in TOOL_SPECS
        .iter()
        .filter(|tool| is_generic_runtime(tool) == generic_runtime)
    {
        let score = tool
            .needles
            .iter()
            .map(|needle| {
                labels
                    .iter()
                    .map(|label| token_occurrences(label, needle))
                    .sum::<usize>()
            })
            .sum();
        if score > best_score {
            best = Some(tool);
            best_score = score;
        }
    }
    best
}

fn is_generic_runtime(tool: &ToolSpec) -> bool {
    matches!(
        tool.display,
        "Node" | "Python" | "Ruby" | "Rust" | "Go" | "Java" | "PHP"
    )
}

fn token_occurrences(text: &str, needle: &str) -> usize {
    text.match_indices(needle)
        .filter(|(start, matched)| token_match(text, *start, matched))
        .count()
}

fn token_match(text: &str, start: usize, matched: &str) -> bool {
    let end = start + matched.len();
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    before.is_none_or(|character| !character.is_ascii_alphanumeric())
        && after.is_none_or(|character| !character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::tool_for_text;

    #[test]
    fn keeps_specific_tools_ahead_of_their_runtimes() {
        assert_eq!(
            tool_for_text("/usr/bin/node ./node_modules/.bin/storybook dev")
                .unwrap()
                .display,
            "Storybook"
        );
        assert_eq!(
            tool_for_text("python3 test-dev-server --server next 4000")
                .unwrap()
                .display,
            "Next.js"
        );
        assert_eq!(
            tool_for_text("node /srv/react-app/node_modules/vite/bin/vite.js")
                .unwrap()
                .display,
            "Vite"
        );
    }

    #[test]
    fn rejects_substring_collisions() {
        assert_eq!(
            tool_for_text("node /srv/nextcloud/server.js")
                .unwrap()
                .display,
            "Node"
        );
        assert_eq!(
            tool_for_text("python /srv/reactor.py").unwrap().display,
            "Python"
        );
        assert!(tool_for_text("bundle exec puma").is_none());
        assert!(tool_for_text("contest runner").is_none());
    }
}
