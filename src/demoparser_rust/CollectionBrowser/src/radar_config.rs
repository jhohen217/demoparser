//! Radar configuration parser for CS2 map radar files

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct RadarConfig {
    pub map_name: String,
    pub material: String,
    pub pos_x: f32,
    pub pos_y: f32,
    pub scale: f32,
    pub vertical_sections: Vec<VerticalSection>,
}

#[derive(Debug, Clone)]
pub struct VerticalSection {
    pub name: String,
    pub altitude_max: f32,
    pub altitude_min: f32,
}

impl RadarConfig {
    /// Parse a radar config file (e.g., de_nuke.txt)
    pub fn from_file(path: &Path) -> Result<Self> {
        eprintln!("LOADING RADAR CONFIG FROM: {}", path.display());
        let content = fs::read_to_string(path)?;
        eprintln!("File loaded, {} bytes", content.len());
        Self::parse(&content)
    }

    /// Parse radar config from string content
    pub fn parse(content: &str) -> Result<Self> {
        let mut map_name = String::new();
        let mut material = String::new();
        let mut pos_x = 0.0;
        let mut pos_y = 0.0;
        let mut scale = 1.0;
        let mut vertical_sections = Vec::new();

        let mut in_vertical_sections = false;
        let mut in_section = false;
        let mut current_section_name = String::new();
        let mut current_altitude_max = 10000.0;
        let mut current_altitude_min = -10000.0;

        eprintln!("=== PARSING RADAR CONFIG ===");

        for line in content.lines() {
            let trimmed = line.trim();

            // Skip comments and empty lines
            if trimmed.starts_with("//") || trimmed.is_empty() {
                continue;
            }

            // Skip opening and closing braces without processing
            if trimmed == "{" {
                continue;
            }

            // Handle closing braces
            if trimmed == "}" {
                // If we're in a vertical section, save it
                if in_section && !current_section_name.is_empty() {
                    vertical_sections.push(VerticalSection {
                        name: current_section_name.clone(),
                        altitude_max: current_altitude_max,
                        altitude_min: current_altitude_min,
                    });
                    current_section_name.clear();
                    current_altitude_max = 10000.0;
                    current_altitude_min = -10000.0;
                    in_section = false;
                } else if in_vertical_sections {
                    // End of verticalsections block
                    in_vertical_sections = false;
                }
                continue;
            }

            // Extract map name from section header (quoted string without tabs)
            if map_name.is_empty()
                && trimmed.starts_with('"')
                && trimmed.ends_with('"')
                && !trimmed.contains('\t')
            {
                map_name = trimmed.trim_matches('"').to_string();
                continue;
            }

            // Check for verticalsections block
            if trimmed == "\"verticalsections\"" {
                in_vertical_sections = true;
                continue;
            }

            // Parse key-value pairs
            if let Some((key, value)) = Self::parse_key_value(trimmed) {
                eprintln!("  Parsed key='{}' value='{}'", key, value);
                if in_vertical_sections && in_section {
                    // We're inside a vertical section
                    match key {
                        "AltitudeMax" => current_altitude_max = value.parse().unwrap_or(10000.0),
                        "AltitudeMin" => current_altitude_min = value.parse().unwrap_or(-10000.0),
                        _ => {}
                    }
                } else if !in_vertical_sections {
                    // We're at the top level
                    match key {
                        "material" => material = value.to_string(),
                        "pos_x" => {
                            pos_x = value.parse().unwrap_or(0.0);
                            eprintln!("    -> Setting pos_x={}", pos_x);
                        }
                        "pos_y" => {
                            pos_y = value.parse().unwrap_or(0.0);
                            eprintln!("    -> Setting pos_y={}", pos_y);
                        }
                        "scale" => {
                            scale = value.parse().unwrap_or(1.0);
                            eprintln!("    -> Setting scale={}", scale);
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // Detect vertical section names (quoted string without tabs, inside verticalsections)
            if in_vertical_sections
                && trimmed.starts_with('"')
                && trimmed.ends_with('"')
                && !trimmed.contains('\t')
            {
                // If we have a previous section, save it first
                if in_section && !current_section_name.is_empty() {
                    vertical_sections.push(VerticalSection {
                        name: current_section_name.clone(),
                        altitude_max: current_altitude_max,
                        altitude_min: current_altitude_min,
                    });
                    current_altitude_max = 10000.0;
                    current_altitude_min = -10000.0;
                }

                current_section_name = trimmed.trim_matches('"').to_string();
                in_section = true;
            }
        }

        // If no vertical sections, add a default one
        if vertical_sections.is_empty() {
            vertical_sections.push(VerticalSection {
                name: "default".to_string(),
                altitude_max: 10000.0,
                altitude_min: -10000.0,
            });
        }

        Ok(RadarConfig {
            map_name,
            material,
            pos_x,
            pos_y,
            scale,
            vertical_sections,
        })
    }

    /// Parse a key-value line from the config
    fn parse_key_value(line: &str) -> Option<(&str, &str)> {
        // Look for pattern: "key"\t+"value" (tab-separated, with possible whitespace)
        if !line.contains('\t') {
            return None;
        }

        let parts: Vec<&str> = line.split('\t').filter(|s| !s.trim().is_empty()).collect();
        if parts.len() < 2 {
            return None;
        }

        // Trim quotes and whitespace from key and value
        let key = parts[0].trim().trim_matches('"');
        let value = parts[1]
            .split("//")
            .next() // Remove inline comments
            .unwrap_or("")
            .trim()
            .trim_matches('"');

        Some((key, value))
    }

    /// Get the appropriate vertical section for a given altitude
    pub fn get_section_for_altitude(&self, altitude: f32) -> Option<&VerticalSection> {
        self.vertical_sections
            .iter()
            .find(|section| altitude > section.altitude_min && altitude <= section.altitude_max)
    }

    /// Transform world coordinates to radar pixel coordinates
    ///
    /// CS2 radar format uses:
    /// - pos_x, pos_y: Upper-left corner of radar in world coordinates
    /// - scale: World units per pixel
    ///
    /// The radar image is 1024x1024 pixels, and we need to:
    /// 1. Transform world coords to radar texture pixels (0-1024 range)
    /// 2. Then scale to the actual canvas size
    pub fn world_to_radar(&self, world_x: f32, world_y: f32, canvas_size: f32) -> (f32, f32) {
        // Step 1: Transform world coordinates to radar texture pixels (0-1024 range)
        // pos_x, pos_y represent the upper-left corner in world coordinates
        // For X: pixels from left = (world_x - upper_left_x) / scale
        let radar_pixel_x = (world_x - self.pos_x) / self.scale;

        // For Y: pixels from top = (upper_left_y - world_y) / scale
        // Note: CS2 Y-axis is inverted (positive Y goes down in screen space)
        let radar_pixel_y = (self.pos_y - world_y) / self.scale;

        // Step 2: The radar image is 1024x1024, so we need to scale from texture pixels to canvas pixels
        // canvas_pixel = (texture_pixel / 1024) * canvas_size
        let radar_image_size = 1024.0;
        let canvas_x = (radar_pixel_x / radar_image_size) * canvas_size;
        let canvas_y = (radar_pixel_y / radar_image_size) * canvas_size;

        (canvas_x, canvas_y)
    }

    /// Transform world coordinates to percentage (0-100) of radar image
    ///
    /// This is similar to Boltobserv's approach where coordinates are expressed
    /// as percentages of the radar image, useful for debugging and comparison.
    ///
    /// Returns (x_percent, y_percent) where 0,0 is top-left and 100,100 is bottom-right
    pub fn world_to_radar_percent(&self, world_x: f32, world_y: f32) -> (f32, f32) {
        const RADAR_IMAGE_SIZE: f32 = 1024.0;

        // Transform world coordinates to radar texture pixels (0-1024 range)
        let radar_pixel_x = (world_x - self.pos_x) / self.scale;
        let radar_pixel_y = (self.pos_y - world_y) / self.scale;

        // Convert to percentage (0-100)
        let percent_x = (radar_pixel_x / RADAR_IMAGE_SIZE) * 100.0;
        let percent_y = (radar_pixel_y / RADAR_IMAGE_SIZE) * 100.0;

        (percent_x, percent_y)
    }

    /// Convert CS2 radar parameters to Boltobserv-style offset and resolution
    ///
    /// This conversion allows understanding the relationship between the two formats:
    /// - CS2: pos_x, pos_y (upper-left), scale
    /// - Boltobserv: offset (from bottom-left to origin), resolution
    ///
    /// Returns (offset_x, offset_y, resolution) in Boltobserv format
    pub fn to_boltobserv_params(&self) -> (f32, f32, f32) {
        const RADAR_IMAGE_SIZE: f32 = 1024.0;

        // In CS2 format:
        // - pos_x, pos_y = upper-left corner in world coords
        // - scale = world units per pixel

        // In Boltobserv format:
        // - offset = distance from bottom-left corner to world origin
        // - resolution = world units per pixel (same as CS2's scale)

        // Calculate bottom-left corner in world coordinates
        let bottom_left_x = self.pos_x;
        let bottom_left_y = self.pos_y - (RADAR_IMAGE_SIZE * self.scale);

        // Offset is the negative of bottom-left position
        // (how far world origin is from bottom-left corner)
        let offset_x = -bottom_left_x;
        let offset_y = -bottom_left_y;
        let resolution = self.scale;

        (offset_x, offset_y, resolution)
    }
}

/// Radar configuration manager
pub struct RadarConfigManager {
    configs: HashMap<String, RadarConfig>,
    radar_dir: PathBuf,
}

impl RadarConfigManager {
    pub fn new(radar_dir: PathBuf) -> Self {
        Self {
            configs: HashMap::new(),
            radar_dir,
        }
    }

    /// Load radar config for a specific map
    pub fn load_config(&mut self, map_name: &str) -> Result<&RadarConfig> {
        // Check if already loaded
        if self.configs.contains_key(map_name) {
            return Ok(&self.configs[map_name]);
        }

        // Try to load from file
        let config_path = self.radar_dir.join(format!("{}.txt", map_name));
        if !config_path.exists() {
            return Err(anyhow!("Radar config not found for map: {}", map_name));
        }

        let config = RadarConfig::from_file(&config_path)?;
        self.configs.insert(map_name.to_string(), config);

        Ok(&self.configs[map_name])
    }

    /// Get a loaded config
    pub fn get_config(&self, map_name: &str) -> Option<&RadarConfig> {
        self.configs.get(map_name)
    }

    /// Get radar image path for a map and section
    pub fn get_radar_image_path(&self, map_name: &str, section: &str) -> PathBuf {
        if section == "default" {
            self.radar_dir.join(format!("{}_radar_psd.png", map_name))
        } else {
            self.radar_dir
                .join(format!("{}_{}_radar_psd.png", map_name, section))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_config() {
        // Test basic parsing functionality
        let content = "\"test_map\"\n{\n\t\"material\"\t\"test\"\n\t\"pos_x\"\t\"100\"\n\t\"pos_y\"\t\"200\"\n\t\"scale\"\t\"5\"\n}\n";
        let config = RadarConfig::parse(content).unwrap();
        assert_eq!(config.map_name, "test_map");
        assert_eq!(config.pos_x, 100.0);
        assert_eq!(config.pos_y, 200.0);
        assert_eq!(config.scale, 5.0);
        // Should have default vertical section
        assert_eq!(config.vertical_sections.len(), 1);
        assert_eq!(config.vertical_sections[0].name, "default");
    }

    #[test]
    fn test_world_to_radar_transformation() {
        // Test with de_nuke parameters
        let config = RadarConfig {
            map_name: "de_nuke".to_string(),
            material: "overviews/de_nuke".to_string(),
            pos_x: -3453.0,
            pos_y: 2887.0,
            scale: 7.0,
            vertical_sections: vec![],
        };

        // Test 1: Upper-left corner (pos_x, pos_y) should map to (0, 0)
        let (x, y) = config.world_to_radar(-3453.0, 2887.0, 1024.0);
        assert!((x - 0.0).abs() < 0.1, "Upper-left X should be 0, got {}", x);
        assert!((y - 0.0).abs() < 0.1, "Upper-left Y should be 0, got {}", y);

        // Test 2: Center of radar (assuming 1024x1024 texture)
        // Center pixel is at 512, so world coord = pos_x + 512*scale, pos_y - 512*scale
        let center_world_x = -3453.0 + (512.0 * 7.0);
        let center_world_y = 2887.0 - (512.0 * 7.0);
        let (x, y) = config.world_to_radar(center_world_x, center_world_y, 1024.0);
        assert!(
            (x - 512.0).abs() < 0.1,
            "Center X should be ~512, got {}",
            x
        );
        assert!(
            (y - 512.0).abs() < 0.1,
            "Center Y should be ~512, got {}",
            y
        );

        // Test 3: Bottom-right corner (pos_x + 1024*scale, pos_y - 1024*scale)
        let br_world_x = -3453.0 + (1024.0 * 7.0);
        let br_world_y = 2887.0 - (1024.0 * 7.0);
        let (x, y) = config.world_to_radar(br_world_x, br_world_y, 1024.0);
        assert!(
            (x - 1024.0).abs() < 0.1,
            "Bottom-right X should be ~1024, got {}",
            x
        );
        assert!(
            (y - 1024.0).abs() < 0.1,
            "Bottom-right Y should be ~1024, got {}",
            y
        );
    }

    #[test]
    fn test_world_to_radar_percent() {
        let config = RadarConfig {
            map_name: "test".to_string(),
            material: "test".to_string(),
            pos_x: -3453.0,
            pos_y: 2887.0,
            scale: 7.0,
            vertical_sections: vec![],
        };

        // Test 1: Upper-left corner should be (0%, 0%)
        let (x_pct, y_pct) = config.world_to_radar_percent(-3453.0, 2887.0);
        assert!((x_pct - 0.0).abs() < 0.1);
        assert!((y_pct - 0.0).abs() < 0.1);

        // Test 2: Bottom-right corner should be (100%, 100%)
        let br_world_x = -3453.0 + (1024.0 * 7.0);
        let br_world_y = 2887.0 - (1024.0 * 7.0);
        let (x_pct, y_pct) = config.world_to_radar_percent(br_world_x, br_world_y);
        assert!((x_pct - 100.0).abs() < 0.1);
        assert!((y_pct - 100.0).abs() < 0.1);

        // Test 3: Center should be (50%, 50%)
        let center_world_x = -3453.0 + (512.0 * 7.0);
        let center_world_y = 2887.0 - (512.0 * 7.0);
        let (x_pct, y_pct) = config.world_to_radar_percent(center_world_x, center_world_y);
        assert!((x_pct - 50.0).abs() < 0.1);
        assert!((y_pct - 50.0).abs() < 0.1);
    }

    #[test]
    fn test_to_boltobserv_params() {
        let config = RadarConfig {
            map_name: "de_nuke".to_string(),
            material: "overviews/de_nuke".to_string(),
            pos_x: -3453.0,
            pos_y: 2887.0,
            scale: 7.0,
            vertical_sections: vec![],
        };

        let (offset_x, offset_y, resolution) = config.to_boltobserv_params();

        // Resolution should match scale
        assert_eq!(resolution, 7.0);

        // Offset should be distance from bottom-left to origin
        // Bottom-left in world coords: (pos_x, pos_y - 1024*scale)
        // = (-3453, 2887 - 7168) = (-3453, -4281)
        // Offset = -bottom_left = (3453, 4281)
        assert_eq!(offset_x, 3453.0);
        assert_eq!(offset_y, 4281.0);
    }

    #[test]
    fn test_vertical_sections() {
        let config = RadarConfig {
            map_name: "de_nuke".to_string(),
            material: "overviews/de_nuke".to_string(),
            pos_x: -3453.0,
            pos_y: 2887.0,
            scale: 7.0,
            vertical_sections: vec![
                VerticalSection {
                    name: "default".to_string(),
                    altitude_max: 10000.0,
                    altitude_min: -495.0,
                },
                VerticalSection {
                    name: "lower".to_string(),
                    altitude_max: -495.0,
                    altitude_min: -10000.0,
                },
            ],
        };

        // Test upper level
        let section = config.get_section_for_altitude(100.0);
        assert!(section.is_some());
        assert_eq!(section.unwrap().name, "default");

        // Test lower level
        let section = config.get_section_for_altitude(-1000.0);
        assert!(section.is_some());
        assert_eq!(section.unwrap().name, "lower");

        // Test boundary
        let section = config.get_section_for_altitude(-495.0);
        assert!(section.is_some());
        assert_eq!(section.unwrap().name, "lower");
    }

    #[test]
    fn test_canvas_scaling() {
        let config = RadarConfig {
            map_name: "test".to_string(),
            material: "test".to_string(),
            pos_x: -3453.0,
            pos_y: 2887.0,
            scale: 7.0,
            vertical_sections: vec![],
        };

        // Test with different canvas sizes to verify scaling works correctly
        let world_x = -3453.0 + (256.0 * 7.0); // 25% from left
        let world_y = 2887.0 - (256.0 * 7.0); // 25% from top

        // With 1024x1024 canvas, should be at 256, 256
        let (x1, y1) = config.world_to_radar(world_x, world_y, 1024.0);
        assert!((x1 - 256.0).abs() < 0.1);
        assert!((y1 - 256.0).abs() < 0.1);

        // With 512x512 canvas, should be at 128, 128 (scaled down)
        let (x2, y2) = config.world_to_radar(world_x, world_y, 512.0);
        assert!((x2 - 128.0).abs() < 0.1);
        assert!((y2 - 128.0).abs() < 0.1);

        // With 2048x2048 canvas, should be at 512, 512 (scaled up)
        let (x3, y3) = config.world_to_radar(world_x, world_y, 2048.0);
        assert!((x3 - 512.0).abs() < 0.1);
        assert!((y3 - 512.0).abs() < 0.1);
    }
}
