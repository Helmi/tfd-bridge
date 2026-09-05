//! Trajectory core adapted from wows-toolkit armor_viewer/ballistics.rs at
//! d1c317e5e10b9e674fb352b159fe81c9cc6e652e (MIT), itself ported from
//! https://github.com/jcw780/wows_shell (MIT), Copyright (c) 2020 jcw780.
//! Adaptation: retain timed horizontal samples and stop at the replay range.
//! See ballistics-LICENSE for the upstream license.
use wowsunpack::game_params::types::Projectile;
const G: f64 = 9.8;
const T0: f64 = 288.15;
const L: f64 = 0.0065;
const P0: f64 = 101325.0;
const R_GAS: f64 = 8.31447;
const M_AIR: f64 = 0.0289644;
const GM_RL: f64 = (G * M_AIR) / (R_GAS * L);
const DT: f64 = 0.02;
const MAX_TIME: f64 = 200.0;
// Toolkit armor-viewer conversion from physical flight seconds to game time.
// Projectile.timeFactor is a different parameter (often 1), not this scale.
pub const TIME_MULTIPLIER: f64 = 2.75;
struct ShellParams {
    v0: f64,
    k: f64,
}

/// Game-time seconds and horizontal progress; the top-down map does not show height.
pub fn trajectory(projectile: &Projectile, pitch: f32, range_m: f64) -> Option<Vec<(f64, f64)>> {
    let mass = projectile.bullet_mass()? as f64;
    let caliber = projectile.bullet_diametr()? as f64;
    let drag = projectile.bullet_air_drag()? as f64;
    let v0 = projectile.bullet_speed()? as f64;
    if ![mass, caliber, v0, range_m]
        .iter()
        .all(|v| v.is_finite() && *v > 0.0)
        || !drag.is_finite()
        || drag < 0.0
        || !pitch.is_finite()
        || pitch.cos() <= 0.0
    {
        return None;
    }
    let params = ShellParams {
        v0,
        k: 0.5 * drag * (caliber / 2.0).powi(2) * std::f64::consts::PI / mass,
    };
    let points = integrate(&params, pitch as f64, range_m)?;
    // Preserve the toolkit's physical-seconds to game-seconds conversion.
    let last = points.len() - 1;
    let count = last.min(24);
    Some(
        (0..=count)
            .map(|i| {
                let (t, x) = points[i * last / count];
                (t / TIME_MULTIPLIER, x / range_m)
            })
            .collect(),
    )
}
fn air_density(altitude: f64) -> f64 {
    let t = T0 - L * altitude;
    if t <= 0.0 {
        return 0.0;
    }
    let p = P0 * (t / T0).powf(GM_RL);
    (M_AIR * p) / (R_GAS * t)
}

/// Compute acceleration components given current state.
/// Returns (ax, ay) where:
///   ax = -k * rho * vx * speed
///   ay = -g - k * rho * vy * speed
fn acceleration(k: f64, vx: f64, vy: f64, y: f64) -> (f64, f64) {
    let rho = air_density(y);
    let speed = (vx * vx + vy * vy).sqrt();
    let k_rho = k * rho;
    let ax = -k_rho * vx * speed;
    let ay = -G - k_rho * vy * speed;
    (ax, ay)
}

/// Simulate a shell trajectory using RK4 integration.
/// Returns timed horizontal positions up to the requested range.
/// Returns None if the shell cannot reach it within MAX_TIME.
fn integrate(params: &ShellParams, launch_angle: f64, range: f64) -> Option<Vec<(f64, f64)>> {
    let mut x: f64 = 0.0;
    let mut y: f64 = 0.0;
    let mut vx = params.v0 * launch_angle.cos();
    let mut vy = params.v0 * launch_angle.sin();
    let mut t: f64 = 0.0;

    let k = params.k;
    let mut points = vec![(0.0, 0.0)];

    while t < MAX_TIME {
        // RK4 integration
        let (ax1, ay1) = acceleration(k, vx, vy, y);

        let vx2 = vx + ax1 * DT * 0.5;
        let vy2 = vy + ay1 * DT * 0.5;
        let y2 = y + vy * DT * 0.5;
        let (ax2, ay2) = acceleration(k, vx2, vy2, y2);

        let vx3 = vx + ax2 * DT * 0.5;
        let vy3 = vy + ay2 * DT * 0.5;
        let y3 = y + vy2 * DT * 0.5;
        let (ax3, ay3) = acceleration(k, vx3, vy3, y3);

        let vx4 = vx + ax3 * DT;
        let vy4 = vy + ay3 * DT;
        let y4 = y + vy3 * DT;
        let (ax4, ay4) = acceleration(k, vx4, vy4, y4);

        let dx = (vx + 2.0 * vx2 + 2.0 * vx3 + vx4) / 6.0 * DT;
        let dy = (vy + 2.0 * vy2 + 2.0 * vy3 + vy4) / 6.0 * DT;
        let dvx = (ax1 + 2.0 * ax2 + 2.0 * ax3 + ax4) / 6.0 * DT;
        let dvy = (ay1 + 2.0 * ay2 + 2.0 * ay3 + ay4) / 6.0 * DT;

        let new_y = y + dy;

        // Stop at the recorded horizontal target range; ship/terrain collision
        // is reconciled with replay impacts separately.
        if x + dx >= range {
            let frac = (range - x) / dx;
            points.push((t + DT * frac, range));
            return Some(points);
        }
        x += dx;
        y = new_y;
        vx += dvx;
        vy += dvy;
        t += DT;
        points.push((t, x));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(drag: f32) -> Projectile {
        Projectile::builder()
            .ammo_type("AP".into())
            .bullet_mass(1000.0)
            .bullet_diametr(0.406)
            .bullet_speed(800.0)
            .bullet_air_drag(drag)
            .time_factor(1.0)
            .build()
    }

    #[test]
    fn vacuum_horizontal_flight_and_game_time_conversion() {
        let points = trajectory(&shell(0.0), 0.2, 10000.0).unwrap();
        let expected = 10000.0 / (800.0 * (0.2_f32 as f64).cos()) / 2.75;
        assert!((points.last().unwrap().0 - expected).abs() < 0.0001);
        assert_eq!(points.first(), Some(&(0.0, 0.0)));
        assert_eq!(points.last().unwrap().1, 1.0);
    }

    #[test]
    fn drag_changes_flight_time_and_horizontal_speed() {
        let vacuum = trajectory(&shell(0.0), 0.2, 10000.0).unwrap();
        let drag = trajectory(&shell(0.3), 0.2, 10000.0).unwrap();
        assert!(drag.last().unwrap().0 > vacuum.last().unwrap().0);
        let first_speed = (drag[1].1 - drag[0].1) / (drag[1].0 - drag[0].0);
        let last = drag.len() - 1;
        let last_speed = (drag[last].1 - drag[last - 1].1) / (drag[last].0 - drag[last - 1].0);
        assert!(first_speed > last_speed);
        assert!(drag.windows(2).all(|p| p[1].0 > p[0].0 && p[1].1 > p[0].1));
    }

    #[test]
    fn invalid_inputs_have_no_physics_guess() {
        assert!(trajectory(&shell(-0.3), 0.2, 10000.0).is_none());
        assert!(trajectory(&shell(0.3), f32::NAN, 10000.0).is_none());
        assert!(trajectory(&shell(0.3), 0.2, 0.0).is_none());
    }
}

