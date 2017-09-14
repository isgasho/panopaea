
use cgmath::{self, InnerSpace, Vector3};
use fft;
use math::{integration, Real};
use ndarray::{Array2, ArrayView2, ArrayViewMut2, Axis};
use num::Zero;
use num::complex::Complex;
use rand;
use rand::distributions::normal;

use std::f32::consts::PI;
use std::sync::Arc;

fn dispersion_peak<T: Real>(gravity: T, wind_speed: T, fetch: T) -> T {
    // Note: pow(x, 1/3) is missing in [Horvath2015]
    T::new(22.0) * (gravity.powi(2) / (wind_speed * fetch)).powf(T::new(1.0/3.0))
}

/// Representing a spectral density function of angular frequency.
///
/// This is only the non-directional component of the spectrum following [Horvath15].
pub trait Spectrum<T: Real>: Sync {
    fn evaluate(&self, omega: T) -> T;
}

/// Joint North Sea Wave Observation Project (JONSWAP) Spectrum [Horvath15] Section 5.1.4
pub struct SpectrumJONSWAP<T: Real> {
    pub wind_speed: T, // [m/s]
    pub fetch: T,
    pub gravity: T,    // [m/s^2]
}

impl<T: Real> Spectrum<T> for SpectrumJONSWAP<T> {
    // [Horvath15] Eq. 28
    fn evaluate(&self, omega: T) -> T {
        if omega < T::default_epsilon() {
            return T::zero();
        }

        let gamma = T::new(3.3);
        let omega_peak = dispersion_peak(self.gravity, self.wind_speed, self.fetch);
        let alpha =
            T::new(0.076) * (self.wind_speed.powi(2) / (self.fetch * self.gravity)).powf(T::new(0.22));
        let sigma = T::new(if omega <= omega_peak { 0.07 } else { 0.09 });
        let r = (-(omega - omega_peak).powi(2) / (T::new(2.0) * (sigma * omega_peak).powi(2))).exp();

        (alpha * (self.gravity).powi(2) / omega.powi(5)) * (T::new(-5.0/4.0) * (omega_peak/omega).powi(4)).exp() * gamma.powf(r)
    }
}

/// Texel MARSEN ARSLOE (TMA) Spectrum [Horvath15] Section 5.1.5
pub struct SpectrumTMA<T: Real> {
    pub jonswap: SpectrumJONSWAP<T>,
    pub depth: T, // TODO: [m]
}

impl<T: Real> SpectrumTMA<T> {
    /// Kitaigorodskii Depth Attenuation Function [Horvath15] Eq. 29
    ///
    /// Using the approximation from Thompson and Vincent, 1983
    /// as proposed in Section 5.1.5.
    fn kitaigorodskii_depth_attenuation(&self, omega: T) -> T {
        let omega_h = (omega * (self.depth / self.jonswap.gravity)).max(T::zero()).min(T::new(2.0));
        if omega_h <= T::one() {
            T::new(0.5) * omega_h.powi(2)
        } else {
            T::one() - T::new(0.5) * (T::new(2.0) - omega_h).powi(2)
        }
    }
}

impl<T: Real> Spectrum<T> for SpectrumTMA<T> {
    fn evaluate(&self, omega: T) -> T {
        self.jonswap.evaluate(omega) * self.kitaigorodskii_depth_attenuation(omega)
    }
}

pub struct Parameters<T> {
    pub surface_tension: T,
    pub water_density: T,
    pub water_depth: T,
    pub gravity: T, // [m/s^2]
    pub wind_speed: T, // [m/s]
    pub fetch: T,
    pub swell: T,
    pub domain_size: T,
}

pub fn build_height_spectrum<S, T>(
    parameters: &Parameters<T>,
    spectrum: &S,
    resolution: usize) -> (Array2<Complex<T>>, Array2<T>)
where
    S: Spectrum<T>,
    T: Real
{
    let pi = T::new(PI);
    let mut height_spectrum = Array2::from_elem((resolution, resolution), Complex::new(T::zero(), T::zero()));
    let mut omega = Array2::zeros((resolution, resolution));

    par_azip!(
        index (j, i),
        mut height_spectrum,
        mut omega,
    in {
        let x = T::new(2 * i as isize - resolution as isize - 1);
        let y = T::new(2 * j as isize - resolution as isize - 1);

        let sample = {
            let k = cgmath::vec2(
                pi * x / parameters.domain_size,
                pi * y / parameters.domain_size,
            );
            sample_spectrum(parameters, spectrum, k)
        };

        *height_spectrum = sample.0;
        *omega = sample.1;
    });

    (height_spectrum, omega)
}

fn sample_spectrum<S, T>(
    parameters: &Parameters<T>,
    spectrum: &S,
    pos: cgmath::Vector2<T>) -> (Complex<T>, T)
where
    S: Spectrum<T>,
    T: Real
{
    if pos.magnitude() < T::default_epsilon() {
        return (Complex::new(T::zero(), T::zero()), T::zero());
    }

    let theta = (pos.y).atan2(pos.x);
    let grad_k = T::new(2.0 * PI) / parameters.domain_size;

    let (omega, grad_omega) = dispersion_capillary(parameters, pos.magnitude());
    let spreading = directional_spreading(parameters, omega, theta, directional_base_donelan_banner);
    let sample = spectrum.evaluate(omega);

    let normal::StandardNormal(z) = rand::random();
    let phase = T::new(2.0 * PI) * rand::random::<T>();

    let amplitude = T::new(z as f32) * (T::new(2.0) * spreading * sample * grad_k.powi(2) * grad_omega / pos.magnitude()).sqrt();

    (Complex::new(phase.cos() * amplitude, phase.sin() * amplitude), omega)
}


fn dispersion_capillary<T>(parameters: &Parameters<T>, wave_number: T) -> (T, T)
where
    T: Real
{
    let sech = |x: T| { T::one() / x.cosh() };

    let sigma = parameters.surface_tension;
    let rho = parameters.water_density;
    let g = parameters.gravity;
    let h = parameters.water_depth;
    let k = wave_number;

    let dispersion = ((g*k + (sigma/rho) * k.powi(3)) * (h*k).tanh()).sqrt();
    let grad_dispersion = (
            h * sech(h*k).powi(2) * (g*k + (sigma/rho) * k.powi(3)) +
            (h*k).tanh() * (g + T::new(3.0)*(sigma/rho) * k.powi(2))
        ) / (T::new(2.0) * dispersion);

    (dispersion, grad_dispersion)
}

// [Horvath15] Eq. 44
fn directional_elongation<T: Real>(parameters: &Parameters<T>, omega: T, theta: T) -> T {
    let shaping = {
        let omega_peak = dispersion_peak(parameters.gravity, parameters.wind_speed, parameters.fetch);
        T::new(16.0) * (omega_peak / omega).tanh() * parameters.swell.powi(2)
    };

    (theta/T::new(2.0)).cos().abs().powf(T::new(2.0)*shaping)
}

fn directional_spreading<F, T>(parameters: &Parameters<T>, omega: T, theta: T, directional_base: F) -> T
where
    F: Fn(&Parameters<T>, T, T) -> T,
    T: Real,
{
    let pi = T::new(PI);
    let normalization =
        integration::trapezoidal_quadrature(
            (-pi, pi),
            128,
            |theta| directional_base(parameters, omega, theta) * directional_elongation(parameters, omega, theta));

    directional_base(parameters, omega, theta) * directional_elongation(parameters, omega, theta) / normalization
}

// Donelan-Banner Directional Spreading [Horvath15] Eq. 38
fn directional_base_donelan_banner<T>(parameters: &Parameters<T>, omega: T, theta: T) -> T
where
    T: Real,
{
    let beta = {
        let omega_peak = dispersion_peak(parameters.gravity, parameters.wind_speed, parameters.fetch);
        let omega_ratio = omega/omega_peak;

        if omega_ratio < T::new(0.95) {
            T::new(2.61) * omega_ratio.powf(T::new(1.3))
        } else if omega_ratio < T::new(1.6) {
            T::new(2.28) * omega_ratio.powf(T::new(-1.3))
        } else {
            let epsilon = T::new(-0.4) + T::new(0.8393) * (T::new(-0.567) * (omega_ratio.powi(2)).ln()).exp();
            T::new(10).powf(epsilon)
        }
    };

    let sech = |x: T| { T::one() / x.cosh() };

    beta / (T::new(2.0) * (beta * T::new(PI)).tanh()) * sech(beta * theta).powi(2)
}

pub struct Ocean<T> {
    resolution: usize,
    fft_plan: fft::FFTplanner<T>,
    fft_buffer: Array2<Complex<T>>,
    displacement_x: Array2<Complex<T>>,
    displacement_y: Array2<Complex<T>>,
    displacement_z: Array2<Complex<T>>,
}

impl<T> Ocean<T> where T: Real + fft::FFTnum {
    pub fn new(resolution: usize) -> Self {
        Ocean {
            fft_plan: fft::FFTplanner::new(true),
            fft_buffer: Array2::from_elem((resolution, resolution), Complex::new(T::zero(), T::zero())),
            resolution,
            displacement_x: Self::new_map(resolution),
            displacement_y: Self::new_map(resolution),
            displacement_z: Self::new_map(resolution),
        }
    }

    fn new_map(resolution: usize) -> Array2<Complex<T>> {
        Array2::from_elem((resolution, resolution), Complex::new(T::zero(), T::zero()))
    }

    pub fn new_displacement(&self) -> Array2<Vector3<T>> {
        Array2::from_elem((self.resolution, self.resolution), Vector3::zero())
    }

    pub fn propagate(
        &mut self,
        time: T,
        parameters: &Parameters<T>,
        samples: ArrayView2<Complex<T>>,
        omega: ArrayView2<T>,
        mut displacement: ArrayViewMut2<Vector3<T>>)
    {
        let resolution = self.resolution;
        let pi = T::new(PI);

        // propgation step
        par_azip!(
            index (j, i),
            omega,
            ref dx (&mut self.displacement_x),
            ref dy (&mut self.displacement_y),
            ref dz (&mut self.displacement_z),
        in {
            let x = T::new(2 * i as isize - resolution as isize - 1);
            let y = T::new(2 * j as isize - resolution as isize - 1);

            let k = cgmath::vec2(
                pi * x / parameters.domain_size,
                pi * y / parameters.domain_size,
            );

            let dispersion = omega * time;
            let disp_pos = Complex::new(dispersion.cos(), dispersion.sin());
            let disp_neg = Complex::new(dispersion.cos(),-dispersion.sin());

            let sample = samples[(j, i)] * disp_pos + samples[(resolution-j-1, resolution-i-1)] * disp_neg;
            let k_normalized = {
                let len = k.magnitude();
                if len < T::default_epsilon() {
                    Complex::new(T::zero(), T::zero())
                } else {
                    Complex::new(k.x / len, k.y / len)
                }
            };

            *dx = Complex::new(T::zero(), -k_normalized.re) * sample;
            *dy = sample;
            *dz = Complex::new(T::zero(), -k_normalized.im) * sample;
        });

        let plan = self.fft_plan.plan_fft(self.resolution);

        Self::spectral_to_spatial(&plan, self.displacement_x.view_mut(), self.fft_buffer.view_mut());
        // correction step
        par_azip!(
            index (j, i),
            src (&self.fft_buffer),
            ref dst (&mut displacement)
        in {
            if (j+i) % 2 == 0 {
                dst.x = -src.re;
            } else {
                dst.x = src.re;
            }
        });

        Self::spectral_to_spatial(&plan, self.displacement_y.view_mut(), self.fft_buffer.view_mut());
        // correction step
        par_azip!(
            index (j, i),
            src (&self.fft_buffer),
            ref dst (&mut displacement)
        in {
            if (j+i) % 2 == 0 {
                dst.y = -src.re;
            } else {
                dst.y = src.re;
            }
        });

        Self::spectral_to_spatial(&plan, self.displacement_z.view_mut(), self.fft_buffer.view_mut());
        // correction step
        par_azip!(
            index (j, i),
            src (&self.fft_buffer),
            ref dst (&mut displacement)
        in {
            if (j+i) % 2 == 0 {
                dst.z = -src.re;
            } else {
                dst.z = src.re;
            }
        });
    }

    // Transform a spatial 2d field into a spatial field
    // Output is stored in self.fft_buffer
    fn spectral_to_spatial(plan: &Arc<fft::FFT<T>>, mut input: ArrayViewMut2<Complex<T>>, mut output: ArrayViewMut2<Complex<T>>) {
        par_azip!(
            mut src (input.axis_iter_mut(Axis(0)))
            mut dst (output.axis_iter_mut(Axis(0)))
        in {
            plan.process(src.as_slice_mut().unwrap(), dst.as_slice_mut().unwrap());
        });

        input.assign(&output.t());

        par_azip!(
            mut src (input.axis_iter_mut(Axis(0)))
            mut dst (output.axis_iter_mut(Axis(0)))
        in {
            plan.process(src.as_slice_mut().unwrap(), dst.as_slice_mut().unwrap());
        });
    }
}
