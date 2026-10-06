use crate::models::DemoDirectory;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration loaded from config.ini
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    pub parser_output: PathBuf,
    pub unzip_dir: Option<PathBuf>,
    pub ram_unzip: bool,
    pub demo_directories: Vec<DemoDirectory>,

    // Parser configuration
    pub aces: bool,
    pub quads: bool,
    pub triples: bool,
    pub multi: bool,
    pub singles: bool,
    pub doubles: bool,
    pub grenade_trajectory_mode: u8,
    pub overwrite: bool,
    pub threads: u32,
    pub output_npz: bool,
    pub output_s2r: bool,

    // Playback configuration
    pub playback_speed: f32,
    pub loop_kill_region: bool,
}

impl BrowserConfig {
    /// Load config.ini from the same directory as the executable
    pub fn load() -> Result<Self> {
        let exe_path = std::env::current_exe().context("Failed to get executable path")?;
        let exe_dir = exe_path
            .parent()
            .context("Failed to get executable directory")?;
        let config_path = exe_dir.join("config.ini");

        Self::load_from_path(&config_path)
    }

    /// Load config from specific path
    pub fn load_from_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            anyhow::bail!("Config file not found: {}", path.display());
        }

        let contents = fs::read_to_string(path)
            .context(format!("Failed to read config file: {}", path.display()))?;

        // Simple INI parsing
        let mut in_paths_section = false;
        let mut in_parser_section = false;
        let mut in_batch_section = false;
        let mut in_directories_section = false;
        let mut in_playback_section = false;
        let mut parser_output = None;
        let mut unzip_dir = None;
        let mut ram_unzip = false;
        let mut demo_directories = Vec::new();

        // Parser settings with defaults
        let mut aces = true;
        let mut quads = false;
        let mut triples = false;
        let mut multi = true;
        let mut singles = true;
        let mut doubles = true;
        let mut grenade_trajectory_mode = 1;
        let mut overwrite = false;
        let mut threads = 0;
        let mut output_npz = true;
        let mut output_s2r = true;

        // Playback settings with defaults
        let mut playback_speed = 1.0;
        let mut loop_kill_region = false;

        for line in contents.lines() {
            let line = line.trim();

            if line.starts_with('[') && line.ends_with(']') {
                in_paths_section = line == "[Paths]";
                in_parser_section = line == "[Parser]";
                in_batch_section = line == "[Batch]";
                in_directories_section = line == "[DemoDirectories]";
                in_playback_section = line == "[Playback]";
            } else if in_paths_section && line.starts_with("ParserOutput") {
                if let Some(value) = line.split('=').nth(1) {
                    parser_output = Some(value.trim().to_string());
                }
            } else if in_paths_section && line.starts_with("unzip_dir") {
                if let Some(value) = line.split('=').nth(1) {
                    let trimmed = value.trim();
                    if !trimmed.is_empty() {
                        unzip_dir = Some(PathBuf::from(trimmed));
                    }
                }
            } else if in_paths_section && line.starts_with("ram_unzip") {
                if let Some(value) = line.split('=').nth(1) {
                    ram_unzip = value.trim().eq_ignore_ascii_case("true");
                }
            } else if in_parser_section && !line.starts_with('#') && line.contains('=') {
                // Parse parser settings
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    match key {
                        "Aces" => aces = value.eq_ignore_ascii_case("true"),
                        "Quads" => quads = value.eq_ignore_ascii_case("true"),
                        "Triples" => triples = value.eq_ignore_ascii_case("true"),
                        "Multi" => multi = value.eq_ignore_ascii_case("true"),
                        "Singles" => singles = value.eq_ignore_ascii_case("true"),
                        "Doubles" => doubles = value.eq_ignore_ascii_case("true"),
                        "grenade_trajectory_mode" => {
                            grenade_trajectory_mode = value.parse().unwrap_or(1);
                        }
                        "overwrite" => overwrite = value.eq_ignore_ascii_case("true"),
                        "output_npz" => output_npz = value.eq_ignore_ascii_case("true"),
                        "output_s2r" => output_s2r = value.eq_ignore_ascii_case("true"),
                        _ => {}
                    }
                }
            } else if in_batch_section && !line.starts_with('#') && line.contains('=') {
                // Parse batch settings
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    if key == "threads" {
                        threads = value.parse().unwrap_or(0);
                    }
                }
            } else if in_playback_section && !line.starts_with('#') && line.contains('=') {
                // Parse playback settings
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    match key {
                        "playback_speed" => {
                            playback_speed = value.parse().unwrap_or(1.0);
                        }
                        "loop_kill_region" => {
                            loop_kill_region = value.eq_ignore_ascii_case("true");
                        }
                        _ => {}
                    }
                }
            } else if in_directories_section && line.contains('=') {
                // Parse directory entries: dir1_path, dir1_enabled, dir1_recursive
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim();

                    if key.ends_with("_path") {
                        let dir_num = key.trim_end_matches("_path");
                        let dir = DemoDirectory::new(PathBuf::from(value));

                        // Look ahead for enabled and recursive flags
                        // We'll set defaults and update them if found
                        demo_directories.push((dir_num.to_string(), dir));
                    } else if key.ends_with("_enabled") {
                        let dir_num = key.trim_end_matches("_enabled");
                        if let Some((_, dir)) =
                            demo_directories.iter_mut().find(|(n, _)| n == dir_num)
                        {
                            dir.enabled = value.eq_ignore_ascii_case("true");
                        }
                    } else if key.ends_with("_recursive") {
                        let dir_num = key.trim_end_matches("_recursive");
                        if let Some((_, dir)) =
                            demo_directories.iter_mut().find(|(n, _)| n == dir_num)
                        {
                            dir.recursive = value.eq_ignore_ascii_case("true");
                        }
                    }
                }
            }
        }

        let parser_output = parser_output.context("Missing ParserOutput in [Paths] section")?;

        Ok(BrowserConfig {
            parser_output: PathBuf::from(parser_output),
            unzip_dir,
            ram_unzip,
            demo_directories: demo_directories.into_iter().map(|(_, d)| d).collect(),
            aces,
            quads,
            triples,
            multi,
            singles,
            doubles,
            grenade_trajectory_mode,
            overwrite,
            threads,
            output_npz,
            output_s2r,
            playback_speed,
            loop_kill_region,
        })
    }

    /// Save config to file
    pub fn save(&self) -> Result<()> {
        let exe_path = std::env::current_exe().context("Failed to get executable path")?;
        let exe_dir = exe_path
            .parent()
            .context("Failed to get executable directory")?;
        let config_path = exe_dir.join("config.ini");

        self.save_to_path(&config_path)
    }

    /// Save config to specific path
    pub fn save_to_path<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();

        // Read existing config to preserve other sections
        let existing_content = if path.exists() {
            fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };

        // Parse existing sections - we'll update [Parser] and [Batch] sections
        let mut preserved_sections = Vec::new();
        let mut parser_section_lines = Vec::new();
        let mut batch_section_lines = Vec::new();
        let mut current_section: Option<(String, Vec<String>)> = None;

        for line in existing_content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                // Save previous section
                if let Some((section_name, section_lines)) = current_section.take() {
                    match section_name.as_str() {
                        "[Parser]" => parser_section_lines = section_lines,
                        "[Batch]" => batch_section_lines = section_lines,
                        "[Paths]" | "[DemoDirectories]" | "[Playback]" => { /* Skip, we'll regenerate */
                        }
                        _ => preserved_sections.push((section_name, section_lines)),
                    }
                }

                // Start new section
                current_section = Some((trimmed.to_string(), Vec::new()));
            } else if let Some((_, ref mut lines)) = current_section {
                // Normalize lines - preserve original whitespace for non-empty lines
                // but don't store lines that are only whitespace
                if line.trim().is_empty() {
                    lines.push(String::new()); // Store as truly empty string
                } else {
                    lines.push(line.to_string());
                }
            }
        }

        // Save last section if needed
        if let Some((section_name, section_lines)) = current_section {
            match section_name.as_str() {
                "[Parser]" => parser_section_lines = section_lines,
                "[Batch]" => batch_section_lines = section_lines,
                "[Paths]" | "[DemoDirectories]" => { /* Skip */ }
                _ => preserved_sections.push((section_name, section_lines)),
            }
        }

        // Build new config content
        let mut contents = String::new();

        // Write Paths section
        contents.push_str("[Paths]\n");
        contents.push_str(&format!(
            "ParserOutput = {}\n",
            self.parser_output.display()
        ));
        if let Some(ref unzip_dir) = self.unzip_dir {
            contents.push_str(&format!("unzip_dir = {}\n", unzip_dir.display()));
        } else {
            contents.push_str("unzip_dir =\n");
        }
        contents.push_str(&format!("ram_unzip = {}\n", self.ram_unzip));
        contents.push_str("\n");

        // Write Parser section with updates
        contents.push_str("[Parser]\n");

        // Trim trailing empty lines before writing
        let mut trimmed_lines = parser_section_lines.clone();
        while trimmed_lines
            .last()
            .map(|l| l.trim().is_empty())
            .unwrap_or(false)
        {
            trimmed_lines.pop();
        }

        for line in &trimmed_lines {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                // Preserve comments
                contents.push_str(line);
                contents.push('\n');
            } else if trimmed.is_empty() {
                // Preserve empty lines (line is already empty, just add newline)
                contents.push('\n');
            } else if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                // Update managed fields, preserve others
                match key {
                    "Aces" => contents.push_str(&format!("Aces = {}\n", self.aces)),
                    "Quads" => contents.push_str(&format!("Quads = {}\n", self.quads)),
                    "Triples" => contents.push_str(&format!("Triples = {}\n", self.triples)),
                    "Multi" => contents.push_str(&format!("Multi = {}\n", self.multi)),
                    "Singles" => contents.push_str(&format!("Singles = {}\n", self.singles)),
                    "Doubles" => contents.push_str(&format!("Doubles = {}\n", self.doubles)),
                    "grenade_trajectory_mode" => contents.push_str(&format!(
                        "grenade_trajectory_mode = {}\n",
                        self.grenade_trajectory_mode
                    )),
                    "overwrite" => contents.push_str(&format!("overwrite = {}\n", self.overwrite)),
                    "output_npz" => {
                        contents.push_str(&format!("output_npz = {}\n", self.output_npz))
                    }
                    "output_s2r" => {
                        contents.push_str(&format!("output_s2r = {}\n", self.output_s2r))
                    }
                    _ => {
                        // Preserve other fields
                        contents.push_str(line);
                        contents.push('\n');
                    }
                }
            }
        }
        contents.push_str("\n");

        // Write preserved sections
        for (section_name, mut section_lines) in preserved_sections {
            // Trim trailing empty lines
            while section_lines
                .last()
                .map(|l| l.trim().is_empty())
                .unwrap_or(false)
            {
                section_lines.pop();
            }

            contents.push_str(&section_name);
            contents.push('\n');
            for line in section_lines {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    // Empty line - just add newline
                    contents.push('\n');
                } else {
                    // Non-empty line - add line content and newline
                    contents.push_str(&line);
                    contents.push('\n');
                }
            }
            contents.push('\n');
        }

        // Write Batch section with updates
        contents.push_str("[Batch]\n");

        // Trim trailing empty lines before writing
        let mut trimmed_batch_lines = batch_section_lines.clone();
        while trimmed_batch_lines
            .last()
            .map(|l| l.trim().is_empty())
            .unwrap_or(false)
        {
            trimmed_batch_lines.pop();
        }

        for line in &trimmed_batch_lines {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                // Preserve comments
                contents.push_str(line);
                contents.push('\n');
            } else if trimmed.is_empty() {
                // Preserve empty lines (line is already empty, just add newline)
                contents.push('\n');
            } else if let Some((key, _)) = trimmed.split_once('=') {
                let key = key.trim();
                if key == "threads" {
                    contents.push_str(&format!("threads = {}\n", self.threads));
                } else {
                    // Preserve other fields
                    contents.push_str(line);
                    contents.push('\n');
                }
            }
        }
        contents.push_str("\n");

        // Write Playback section
        contents.push_str("[Playback]\n");
        contents.push_str(&format!("playback_speed = {}\n", self.playback_speed));
        contents.push_str(&format!("loop_kill_region = {}\n", self.loop_kill_region));
        contents.push_str("\n");

        // Write DemoDirectories section
        if !self.demo_directories.is_empty() {
            contents.push_str("[DemoDirectories]\n");
            for (i, dir) in self.demo_directories.iter().enumerate() {
                let num = i + 1;
                contents.push_str(&format!("dir{}_path = {}\n", num, dir.path.display()));
                contents.push_str(&format!("dir{}_enabled = {}\n", num, dir.enabled));
                contents.push_str(&format!("dir{}_recursive = {}\n", num, dir.recursive));
            }
        }

        fs::write(path, contents)
            .context(format!("Failed to write config file: {}", path.display()))?;

        Ok(())
    }

    /// Get the KillCollectionMaster directory path
    pub fn kill_collection_master_dir(&self) -> PathBuf {
        self.parser_output.join("KillCollectionMaster")
    }
}
