//! Tag manipulation utility functions

/// Appends a new tag to an existing tag string.
/// Handles both array format `[a, b]` and legacy comma-separated formats.
pub fn append_tag(current: &str, new_tag: &str) -> String {
    let trimmed = current.trim();
    if trimmed.is_empty() {
        return new_tag.to_string();
    }

    // Check if it's already an array format [a, b]
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let content = &trimmed[1..trimmed.len() - 1];
        if content.is_empty() {
            return format!("[{}]", new_tag);
        }
        // Simple check if tag exists (could be improved to parse properly)
        if content.contains(new_tag) {
            return current.to_string();
        }
        return format!("[{}, {}]", content, new_tag);
    }

    // If it's a plain string, treat as comma separated or single value
    // Check if new_tag is already present
    if trimmed == new_tag
        || trimmed.contains(&format!(", {}", new_tag))
        || trimmed.contains(&format!("{},", new_tag))
    {
        return current.to_string();
    }

    // Convert to array format if we are appending
    // Assuming we want to migrate to array format
    format!("[{}, {}]", trimmed, new_tag)
}

/// Removes a specific tag from a tag string.
/// Handles both array format `[a, b]` and legacy comma-separated formats.
pub fn remove_tag(current: &str, tag_to_remove: &str) -> String {
    let trimmed = current.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Check if it's array format
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let content = &trimmed[1..trimmed.len() - 1];
        let tags: Vec<&str> = content
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != tag_to_remove)
            .collect();

        if tags.is_empty() {
            return String::new();
        }
        return format!("[{}]", tags.join(", "));
    }

    // Legacy format (comma separated or single)
    let tags: Vec<&str> = trimmed
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != tag_to_remove)
        .collect();

    if tags.is_empty() {
        return String::new();
    }
    // Convert to array format if we are modifying it
    format!("[{}]", tags.join(", "))
}
