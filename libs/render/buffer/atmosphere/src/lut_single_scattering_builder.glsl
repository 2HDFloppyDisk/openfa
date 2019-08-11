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
#include "include_atmosphere.glsl"
#include "lut_shared_builder.glsl"


void
compute_single_scattering_integrand(
    AtmosphereParameters atmosphere,
    sampler2D transmittance_texture,
    ScatterCoord coord,
    float d,
    bool ray_r_mu_intersects_ground,
    out vec4 rayleigh,
    out vec4 mie
) {
    float altitude = sqrt(d * d + 2.0 * coord.r * coord.mu * d + coord.r * coord.r);
    float r_d = clamp_radius(altitude, atmosphere.bottom_radius, atmosphere.top_radius);
    float mu_s_d = clamp_cosine((coord.r * coord.mu_s + d * coord.nu) / r_d);
    vec4 base_transmittance = get_transmittance(
        transmittance_texture,
        coord.r, coord.mu,
        d,
        ray_r_mu_intersects_ground,
        atmosphere.bottom_radius,
        atmosphere.top_radius
    );
    vec4 transmittance_to_sun = get_transmittance_to_sun(
        transmittance_texture,
        r_d,
        mu_s_d,
        atmosphere.bottom_radius,
        atmosphere.top_radius,
        atmosphere.sun_angular_radius
    );
    vec4 transmittance = base_transmittance * transmittance_to_sun;
    rayleigh = transmittance * get_profile_density(atmosphere.rayleigh_density, r_d - atmosphere.bottom_radius);
    mie = transmittance * get_profile_density(atmosphere.mie_density, r_d - atmosphere.bottom_radius);
}

void
compute_single_scattering(
    AtmosphereParameters atmosphere,
    sampler2D transmittance_texture,
    ScatterCoord coord,
    bool ray_r_mu_intersects_ground,
    out vec4 rayleigh,
    out vec4 mie
) {
    // assert(coord.r >= atmosphere.bottom_radius && coord.r <= atmosphere.top_radius);
    // assert(coord.mu >= -1.0 && coord.mu <= 1.0);
    // assert(coord.mu_s >= -1.0 && coord.mu_s <= 1.0);
    // assert(coord.nu >= -1.0 && coord.nu <= 1.0);

    // Number of intervals for the numerical integration.
    const int SAMPLE_COUNT = 50;
    // The integration step, i.e. the length of each integration interval.
    float path_length = distance_to_nearest_atmosphere_boundary(
        vec2(coord.r, coord.mu),
        atmosphere.bottom_radius,
        atmosphere.top_radius,
        ray_r_mu_intersects_ground
    );
    float dx =  path_length / float(SAMPLE_COUNT);
    // Integration loop.
    vec4 rayleigh_sum = vec4(0.0);
    vec4 mie_sum = vec4(0.0);
    for (int i = 0; i <= SAMPLE_COUNT; ++i) {
        float d_i = float(i) * dx;
        // The Rayleigh and Mie single scattering at the current sample point.
        vec4 rayleigh_i;
        vec4 mie_i;
        compute_single_scattering_integrand(
            atmosphere,
            transmittance_texture,
            coord,
            d_i,
            ray_r_mu_intersects_ground,
            rayleigh_i,
            mie_i
        );
        // Sample weight (from the trapezoidal rule).
        float weight_i = (i == 0 || i == SAMPLE_COUNT) ? 0.5 : 1.0;
        rayleigh_sum += rayleigh_i * weight_i;
        mie_sum += mie_i * weight_i;
    }
    rayleigh = rayleigh_sum * dx * atmosphere.sun_irradiance * atmosphere.rayleigh_scattering_coefficient;
    mie = mie_sum * dx * atmosphere.sun_irradiance * atmosphere.mie_scattering_coefficient;
}

void
compute_single_scattering_program(
    vec3 sample_coord,
    mat4 rad_to_lum,
    AtmosphereParameters atmosphere,
    sampler2D transmittance_texture,
    restrict writeonly image3D delta_rayleigh_scattering_texture,
    restrict writeonly image3D delta_mie_scattering_texture,
    out vec3 scattering,
    out vec3 single_mie_scattering
) {
    bool ray_r_mu_intersects_ground;
    ScatterCoord coord = scattering_frag_coord_to_rmumusnu(
        sample_coord,
        atmosphere,
        ray_r_mu_intersects_ground
    );

    vec4 delta_rayleigh;
    vec4 delta_mie;
    compute_single_scattering(
        atmosphere,
        transmittance_texture,
        coord,
        ray_r_mu_intersects_ground,
        delta_rayleigh,
        delta_mie
    );

    ivec3 frag_coord = ivec3(sample_coord);
    imageStore(
        delta_rayleigh_scattering_texture,
        frag_coord,
        delta_rayleigh
    );
    imageStore(
        delta_mie_scattering_texture,
        frag_coord,
        delta_mie
    );

    scattering = vec3(rad_to_lum * delta_rayleigh);
    single_mie_scattering = vec3(rad_to_lum * delta_mie);
}