//! Rerun logging helpers shared by the visual examples.
//!
//! Every scene is logged twice: once under `enu/` as local metric geometry
//! laid flat on `z = 0`, and once under `wgs/` as lat/lon so the viewer can
//! put a map underneath. The two are the same numbers through the workspace
//! datum, so a scene that looks right in one and wrong in the other means the
//! datum is wrong.

#![allow(dead_code)]

use std::error::Error;

use concord::{Enu, to_wgs_from_enu};
use datapod::{Geo, Point};
use rerun::{
    Color, GeoLineStrings, GeoPoints, LineStrips3D, Points3D, RecordingStream,
    RecordingStreamBuilder,
};

/// Where the recording goes, in order of preference:
///
/// - `SYNCBOT_VIZ_RRD` — write to that file and never open a window. This is
///   what a headless box or a failing test wants.
/// - `RERUN_URL` — attach to a viewer someone already has open.
/// - otherwise spawn a viewer.
///
/// Empty values count as unset, so `RERUN_URL=` in a script means "spawn"
/// rather than "connect to the empty string".
pub fn connect(app_id: &str) -> Result<RecordingStream, Box<dyn Error>> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    if let Some(path) = env("SYNCBOT_VIZ_RRD") {
        return Ok(RecordingStreamBuilder::new(app_id).save(path)?);
    }
    if let Some(url) = env("RERUN_URL") {
        return Ok(RecordingStreamBuilder::new(app_id).connect_grpc_opts(url)?);
    }
    Ok(RecordingStreamBuilder::new(app_id).spawn()?)
}

pub fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb(r, g, b)
}

/// The same colour at `factor` brightness — for a resource that is held
/// loosely rather than exclusively, or a machine that has gone quiet.
pub fn scaled((r, g, b): (u8, u8, u8), factor: f32) -> Color {
    let dim = |c: u8| (c as f32 * factor).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb(dim(r), dim(g), dim(b))
}

pub fn lat_lon(point: Point, datum: Geo) -> [f64; 2] {
    let wgs = to_wgs_from_enu(Enu::new(point.x, point.y, point.z, datum));
    [wgs.latitude, wgs.longitude]
}

// ------------------------------------------------------------------- 3D / ENU

pub fn log_polygon_3d(
    rec: &RecordingStream,
    path: &str,
    vertices: &[Point],
    color: Color,
    radius: f32,
) -> Result<(), Box<dyn Error>> {
    if vertices.len() < 2 {
        return Ok(());
    }
    rec.log(
        path,
        &LineStrips3D::new([close(points3(vertices))])
            .with_colors([color])
            .with_radii([radius]),
    )?;
    Ok(())
}

pub fn log_polylines_3d(
    rec: &RecordingStream,
    path: &str,
    polylines: &[Vec<Point>],
    color: Color,
    radius: f32,
) -> Result<(), Box<dyn Error>> {
    let strips: Vec<Vec<[f32; 3]>> = polylines
        .iter()
        .filter(|line| line.len() >= 2)
        .map(|line| points3(line))
        .collect();
    if strips.is_empty() {
        return Ok(());
    }
    let colors: Vec<Color> = strips.iter().map(|_| color).collect();
    let radii: Vec<f32> = strips.iter().map(|_| radius).collect();
    rec.log(
        path,
        &LineStrips3D::new(strips)
            .with_colors(colors)
            .with_radii(radii),
    )?;
    Ok(())
}

pub fn log_points_3d(
    rec: &RecordingStream,
    path: &str,
    points: &[Point],
    labels: &[String],
    color: Color,
    radius: f32,
) -> Result<(), Box<dyn Error>> {
    let positions = points3(points);
    if positions.is_empty() {
        return Ok(());
    }
    let colors: Vec<Color> = positions.iter().map(|_| color).collect();
    let radii: Vec<f32> = positions.iter().map(|_| radius).collect();
    let mut points = Points3D::new(positions)
        .with_colors(colors)
        .with_radii(radii);
    if !labels.is_empty() {
        points = points.with_labels(labels.to_vec());
    }
    rec.log(path, &points)?;
    Ok(())
}

/// A machine as a footprint plus a nose spur, so heading reads at a glance.
pub fn log_machine_3d(
    rec: &RecordingStream,
    path: &str,
    at: Point,
    yaw: f64,
    color: Color,
    length: f64,
    width: f64,
) -> Result<(), Box<dyn Error>> {
    let (cos, sin) = (yaw.cos(), yaw.sin());
    let place = |lx: f64, ly: f64| {
        [
            (at.x + cos * lx - sin * ly) as f32,
            (at.y + sin * lx + cos * ly) as f32,
            0.0f32,
        ]
    };
    let (half_l, half_w) = (length * 0.5, width * 0.5);
    let body = vec![
        place(half_l, half_w),
        place(half_l, -half_w),
        place(-half_l, -half_w),
        place(-half_l, half_w),
        place(half_l, half_w),
    ];
    rec.log(
        format!("{path}/body"),
        &LineStrips3D::new([body])
            .with_colors([color])
            .with_radii([(width * 0.09) as f32]),
    )?;
    let nose = vec![place(half_l, 0.0), place(half_l + length * 0.45, 0.0)];
    rec.log(
        format!("{path}/heading"),
        &LineStrips3D::new([nose])
            .with_colors([color])
            .with_radii([(width * 0.12) as f32]),
    )?;
    Ok(())
}

// ------------------------------------------------------------------- geo / WGS

pub fn log_polygon_geo(
    rec: &RecordingStream,
    path: &str,
    vertices: &[Point],
    datum: Geo,
    color: Color,
) -> Result<(), Box<dyn Error>> {
    if vertices.len() < 2 {
        return Ok(());
    }
    let strip = close(points_geo(vertices, datum));
    rec.log(
        path,
        &GeoLineStrings::from_lat_lon([strip]).with_colors([color]),
    )?;
    Ok(())
}

pub fn log_polylines_geo(
    rec: &RecordingStream,
    path: &str,
    polylines: &[Vec<Point>],
    datum: Geo,
    color: Color,
) -> Result<(), Box<dyn Error>> {
    let strips: Vec<Vec<[f64; 2]>> = polylines
        .iter()
        .filter(|line| line.len() >= 2)
        .map(|line| points_geo(line, datum))
        .collect();
    if strips.is_empty() {
        return Ok(());
    }
    let colors: Vec<Color> = strips.iter().map(|_| color).collect();
    rec.log(
        path,
        &GeoLineStrings::from_lat_lon(strips).with_colors(colors),
    )?;
    Ok(())
}

pub fn log_points_geo(
    rec: &RecordingStream,
    path: &str,
    points: &[Point],
    datum: Geo,
    color: Color,
    radius: f32,
) -> Result<(), Box<dyn Error>> {
    if points.is_empty() {
        return Ok(());
    }
    let positions = points_geo(points, datum);
    let colors: Vec<Color> = positions.iter().map(|_| color).collect();
    let radii: Vec<f32> = positions.iter().map(|_| radius).collect();
    rec.log(
        path,
        &GeoPoints::from_lat_lon(positions)
            .with_colors(colors)
            .with_radii(radii),
    )?;
    Ok(())
}

// ------------------------------------------------------------------- internals

fn points3(points: &[Point]) -> Vec<[f32; 3]> {
    points
        .iter()
        .map(|p| [p.x as f32, p.y as f32, p.z as f32])
        .collect()
}

fn points_geo(points: &[Point], datum: Geo) -> Vec<[f64; 2]> {
    points.iter().map(|p| lat_lon(*p, datum)).collect()
}

fn close<T: Copy + PartialEq>(mut strip: Vec<T>) -> Vec<T> {
    if let Some(&first) = strip.first()
        && strip.last() != Some(&first)
    {
        strip.push(first);
    }
    strip
}
