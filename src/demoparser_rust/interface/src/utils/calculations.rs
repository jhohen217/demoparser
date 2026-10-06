//! Calculations module for the interface crate
//!
//! This module contains utility functions for calculations.

/// Calculate the distance between two points
pub fn calculate_distance(point1: &[f32; 3], point2: &[f32; 3]) -> f32 {
    let dx = point2[0] - point1[0];
    let dy = point2[1] - point1[1];
    let dz = point2[2] - point1[2];

    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Calculate the radius of a set of points
pub fn calculate_radius(points: &[[f32; 3]]) -> f32 {
    if points.is_empty() {
        return 0.0;
    }

    // Calculate the centroid
    let mut centroid = [0.0, 0.0, 0.0];

    for point in points {
        centroid[0] += point[0];
        centroid[1] += point[1];
        centroid[2] += point[2];
    }

    centroid[0] /= points.len() as f32;
    centroid[1] /= points.len() as f32;
    centroid[2] /= points.len() as f32;

    // Calculate the maximum distance from the centroid
    let mut max_distance = 0.0;

    for point in points {
        let distance = calculate_distance(&centroid, point);

        if distance > max_distance {
            max_distance = distance;
        }
    }

    max_distance
}

/// Calculate the total distance traveled through a set of points
pub fn calculate_total_distance(points: &[[f32; 3]]) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }

    let mut total_distance = 0.0;

    for i in 0..points.len() - 1 {
        total_distance += calculate_distance(&points[i], &points[i + 1]);
    }

    total_distance
}
