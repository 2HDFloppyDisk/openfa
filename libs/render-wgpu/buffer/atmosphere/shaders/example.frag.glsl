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
#version 450

#include <common/include/include_global.glsl>

layout(location = 0) in vec3 v_ray;
layout(location = 1) in vec3 v_camera;
layout(location = 2) in vec3 v_sun_direction;
layout(location = 0) out vec4 f_color;

#include <buffer/atmosphere/include/common.glsl>

const float EXPOSURE = MAX_LUMINOUS_EFFICACY * 0.0001;

#include <buffer/atmosphere/include/descriptorset.glsl>
#include <buffer/atmosphere/include/draw_atmosphere.glsl>

void main() {
    vec3 view = normalize(v_ray);

    vec3 ground_radiance;
    float ground_alpha;
    compute_ground_radiance(
        atmosphere,
        transmittance_texture,
        transmittance_sampler,
        scattering_texture,
        scattering_sampler,
        single_mie_scattering_texture,
        single_mie_scattering_sampler,
        irradiance_texture,
        irradiance_sampler,
        v_camera,
        view,
        v_sun_direction,
        ground_radiance,
        ground_alpha);

    vec3 sky_radiance = vec3(0);
    compute_sky_radiance(
        atmosphere,
        transmittance_texture,
        transmittance_sampler,
        scattering_texture,
        scattering_sampler,
        single_mie_scattering_texture,
        single_mie_scattering_sampler,
        irradiance_texture,
        irradiance_sampler,
        v_camera,
        view,
        v_sun_direction,
        sky_radiance
    );

    vec3 radiance = sky_radiance;
    radiance = mix(radiance, ground_radiance, ground_alpha);

    vec3 color = pow(
            vec3(1.0) - exp(-radiance / vec3(atmosphere.whitepoint) * EXPOSURE),
            vec3(1.0 / 2.2)
        );
    f_color = vec4(color, 1.0);

    //f_color = vec4(1.0, 0.0, 1.0, 1.0);
    //f_color = vec4(view, 1.0);
}
