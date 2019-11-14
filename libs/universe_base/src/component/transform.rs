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
use nalgebra::{Point3, UnitQuaternion};
use specs::{Component, VecStorage};

pub struct Transform {
    position: Point3<f32>,
    rotation: UnitQuaternion<f32>,
    // scale: Vector3<f64>, // we don't have an upload slot for this currently.
}

impl Component for Transform {
    type Storage = VecStorage<Self>;
}

impl Transform {
    pub fn new(position: Point3<f32>, rotation: UnitQuaternion<f32>) -> Self {
        Self { position, rotation }
    }

    // Convert to dense pack for upload.
    pub fn compact(&self) -> [f32; 6] {
        let (a, b, c) = self.rotation.euler_angles();
        [self.position.x, self.position.y, self.position.z, a, b, c]
    }
}