// This file is part of OpenFA.
//
// OpenFA is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// OpenFA is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with OpenFA.  If not, see <http://www.gnu.org/licenses/>.
use crate::{debug_vertex::DebugVertex, patch_vertex::PatchVertex};
use absolute_unit::{Kilometers, Radians};
use camera::ArcBallCamera;
use geodesy::{Cartesian, GeoCenter, Graticule};
use geometry::{
    algorithm::{compute_normal, solid_angle},
    intersect,
    intersect::{CirclePlaneIntersection, PlaneSide, SpherePlaneIntersection},
    IcoSphere, Plane, Sphere,
};
use memoffset::offset_of;
use nalgebra::{Point3, Vector3};
use std::{
    cell::RefCell,
    cmp::{Ord, Ordering},
    collections::BinaryHeap,
    f64::consts::PI,
    fmt, mem,
    ops::Range,
    sync::Arc,
};
use universe::{EARTH_RADIUS_KM, EVEREST_HEIGHT_KM};
use wgpu;
use zerocopy::{AsBytes, FromBytes};

// We introduce a substantial amount of error in our intersection computations below
// with all the dot products and re-normalizations. This is fine, as long as we use a
// large enough offset when comparing near zero to get stable results and that pad
// extends the collisions in the right direction.
const SIDEDNESS_OFFSET: f64 = -1f64;

#[derive(Debug, Copy, Clone)]
pub(crate) struct Patch {
    solid_angle: f64,
    normal: Vector3<f64>, // at center of patch

    // In geocentric, cartesian kilometers
    pts: [Point3<f64>; 3],

    // Planes
    planes: [Plane<f64>; 3],

    level: u16,
    tombstone: bool,
}

impl Patch {
    pub(crate) fn new() -> Self {
        Self {
            level: 0,
            tombstone: true,
            solid_angle: 0f64,
            normal: Vector3::new(0f64, 1f64, 0f64),
            pts: [
                Point3::new(0f64, 0f64, 0f64),
                Point3::new(0f64, 0f64, 0f64),
                Point3::new(0f64, 0f64, 0f64),
            ],
            planes: [Plane::xy(), Plane::xy(), Plane::xy()],
        }
    }

    pub(crate) fn change_target(&mut self, level: u16, pts: [Point3<f64>; 3]) {
        self.tombstone = false;
        self.level = level;
        self.pts = pts;
        self.normal = compute_normal(&pts[0], &pts[1], &pts[2]);
        let origin = Point3::new(0f64, 0f64, 0f64);
        self.planes = [
            Plane::from_point_and_normal(&pts[0], &compute_normal(&pts[1], &origin, &pts[0])),
            Plane::from_point_and_normal(&pts[1], &compute_normal(&pts[2], &origin, &pts[1])),
            Plane::from_point_and_normal(&pts[2], &compute_normal(&pts[0], &origin, &pts[2])),
        ];
        assert!(self.planes[0].point_is_in_front(&pts[2]));
        assert!(self.planes[1].point_is_in_front(&pts[0]));
        assert!(self.planes[2].point_is_in_front(&pts[1]));
        self.solid_angle = 0f64;
    }

    pub(crate) fn is_alive(&self) -> bool {
        !self.tombstone
    }

    pub(crate) fn erect_tombstone(&mut self) {
        self.tombstone = true;
        self.solid_angle = -1f64;
    }

    pub(crate) fn recompute_solid_angle(
        &mut self,
        eye_position: &Point3<f64>,
        eye_direction: &Vector3<f64>,
    ) {
        self.solid_angle = solid_angle(&eye_position, &eye_direction, &self.pts);
    }

    pub(crate) fn level(&self) -> usize {
        self.level as usize
    }

    pub(crate) fn points(&self) -> &[Point3<f64>; 3] {
        &self.pts
    }

    pub(crate) fn point(&self, offset: usize) -> &Point3<f64> {
        &self.pts[offset]
    }

    fn goodness(&self) -> f64 {
        self.solid_angle
    }

    pub(crate) fn distance_squared_to(&self, point: &Point3<f64>) -> f64 {
        let mut minimum = 99999999f64;

        // bottom points
        for p in &self.pts {
            let v = p - point;
            let d = v.dot(&v);
            if d < minimum {
                minimum = d;
            }
        }
        // top points
        for p in &self.pts {
            let top_point = p + (p.coords.normalize() * EARTH_RADIUS_KM);
            let v = top_point - point;
            let d = v.dot(&v);
            if d < minimum {
                minimum = d;
            }
        }

        return minimum;
    }

    fn is_behind_plane(&self, plane: &Plane<f64>, show_msgs: bool) -> bool {
        // Patch Extent:
        //   outer: the three planes cutting from geocenter through each pair of points in vertices.
        //   bottom: radius of the planet
        //   top: radius of planet from height of everest

        // Two phases:
        //   1) Convex hull over points
        //   2) Plane-sphere for convex top area

        // bottom points
        for p in &self.pts {
            if plane.point_is_in_front_with_offset(&p, SIDEDNESS_OFFSET) {
                return false;
            }
        }
        // top points
        for p in &self.pts {
            let top_point = p + (p.coords.normalize() * EARTH_RADIUS_KM);
            if plane.point_is_in_front_with_offset(&top_point, SIDEDNESS_OFFSET) {
                return false;
            }
        }

        // plane vs top sphere
        let top_sphere = Sphere::from_center_and_radius(
            &Point3::new(0f64, 0f64, 0f64),
            EVEREST_HEIGHT_KM + EVEREST_HEIGHT_KM,
        );
        let intersection = intersect::sphere_vs_plane(&top_sphere, &plane);
        match intersection {
            SpherePlaneIntersection::NoIntersection { side, .. } => side == PlaneSide::Above,
            SpherePlaneIntersection::Intersection(ref circle) => {
                for (i, plane) in self.planes.iter().enumerate() {
                    let intersect = intersect::circle_vs_plane(circle, plane, SIDEDNESS_OFFSET);
                    match intersect {
                        CirclePlaneIntersection::Parallel => {
                            if show_msgs {
                                println!("  parallel {}", i);
                            }
                        }
                        CirclePlaneIntersection::BehindPlane => {
                            if show_msgs {
                                println!("  outside {}", i);
                            }
                        }
                        CirclePlaneIntersection::Tangent(ref p) => {
                            if self.point_is_in_cone(p) {
                                if show_msgs {
                                    println!("  tangent {} in cone: {}", i, p);
                                }
                                return false;
                            }
                            if show_msgs {
                                println!("  tangent {} NOT in cone: {}", i, p);
                            }
                        }
                        CirclePlaneIntersection::Intersection(ref p0, ref p1) => {
                            if self.point_is_in_cone(p0) || self.point_is_in_cone(p1) {
                                if show_msgs {
                                    println!("  intersection {} in cone: {}, {}", i, p0, p1);
                                }
                                return false;
                            }
                            if show_msgs {
                                println!("  intersection {} NOT in cone: {}, {}", i, p0, p1);
                            }
                        }
                        CirclePlaneIntersection::InFrontOfPlane => {
                            if self.point_is_in_cone(circle.center()) {
                                if show_msgs {
                                    println!("  circle {} in cone: {}", i, circle.center());
                                }
                                return false;
                            }
                            if show_msgs {
                                println!("  circle {} NOT in cone: {}", i, circle.center());
                            }
                        }
                    }
                }

                if show_msgs {
                    println!("  fell out of all planes");
                }
                // No test was in front of the plane, so we are fully behind it.
                true
            }
        }
    }

    fn point_is_in_cone(&self, point: &Point3<f64>) -> bool {
        for plane in &self.planes {
            if !plane.point_is_in_front_with_offset(point, SIDEDNESS_OFFSET) {
                return false;
            }
        }
        true
    }

    fn is_back_facing(&self, eye_position: &Point3<f64>) -> bool {
        for p in &self.pts {
            if (p - eye_position).dot(&self.normal) <= -0.00001f64 {
                return false;
            }
        }
        true
    }

    pub(crate) fn keep(
        &self,
        camera: &ArcBallCamera,
        horizon_plane: &Plane<f64>,
        eye_position: &Point3<f64>,
    ) -> bool {
        // Cull back-facing
        if self.is_back_facing(eye_position) {
            // println!("  no - back facing");
            return false;
        }

        // Cull below horizon
        if self.is_behind_plane(&horizon_plane, false) {
            //println!("  no - below horizon");
            return false;
        }

        // Cull outside the view frustum
        for plane in &camera.world_space_frustum() {
            if self.is_behind_plane(plane, false) {
                return false;
            }
        }

        true
    }
}

impl Eq for Patch {}

impl PartialEq for Patch {
    fn eq(&self, other: &Self) -> bool {
        self.goodness() == other.goodness()
    }
}

impl PartialOrd for Patch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.goodness().partial_cmp(&other.goodness())
    }
}
impl Ord for Patch {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Less)
    }
}
