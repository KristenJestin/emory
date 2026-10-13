//! Where a model really computes, and what to do when the GPU cannot be had.
//!
//! A server must never run on the CPU without anyone knowing. Asking onnxruntime for CUDA is not
//! enough to be sure: when it cannot load the provider, a build or a library may fall back to the
//! CPU by itself, with one line on stderr. So the placement is checked after the model is loaded
//! ([`verify`]), and a placement that fails is either an error at start or, if the configuration
//! allows it (`fallback_to_cpu`), a load on the CPU announced by a warning ([`place`]).

use anyhow::{Context, Result};
use tracing::warn;

use crate::config::Provider;

/// The library onnxruntime loads when, and only when, it puts a model on the GPU.
const CUDA_PROVIDER_LIBRARY: &str = "libonnxruntime_providers_cuda";

/// Checks that the process is computing where `provider` says. A CPU model is always where it
/// should be. For CUDA, the provider library of onnxruntime must be loaded in the process: if it
/// is not, the model went to the CPU. Where this cannot be known (not Linux, `/proc` unreadable)
/// there is no verdict, and the load is accepted.
pub fn verify(provider: Provider) -> Result<()> {
    if provider != Provider::Cuda {
        return Ok(());
    }
    let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
        return Ok(());
    };
    if cuda_provider_is_mapped(&maps) {
        Ok(())
    } else {
        anyhow::bail!(
            "the model was loaded, but onnxruntime did not load its CUDA provider \
             (the driver, CUDA 13 or cuDNN 9 libraries are missing or unusable): it would \
             compute on the CPU"
        )
    }
}

/// Whether a `/proc/<pid>/maps` listing holds the CUDA provider library of onnxruntime.
fn cuda_provider_is_mapped(maps: &str) -> bool {
    maps.lines()
        .any(|line| line.contains(CUDA_PROVIDER_LIBRARY))
}

/// Loads something on `wanted`; if that fails and `fallback_to_cpu` is set, loads it on the CPU
/// after a warning. Returns what was loaded and where it really is. Without the opt-in the error
/// of the first attempt is the error of the start: there is no quiet way to the CPU.
pub fn place<T>(
    what: &str,
    wanted: Provider,
    fallback_to_cpu: bool,
    mut load: impl FnMut(Provider) -> Result<T>,
) -> Result<(T, Provider)> {
    match load(wanted) {
        Ok(loaded) => Ok((loaded, wanted)),
        Err(error) if wanted != Provider::Cpu && fallback_to_cpu => {
            warn!(
                wanted = wanted.name(),
                running_on = "cpu",
                "{what}: {} is unavailable, FALLING BACK TO THE CPU (fallback_to_cpu = true): {error:#}",
                wanted.name()
            );
            let loaded = load(Provider::Cpu)
                .with_context(|| format!("{what}: the CPU fallback failed too"))?;
            Ok((loaded, Provider::Cpu))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITHOUT: &str = "\
55d1c0a00000-55d1c0a40000 r-xp 00000000 00:00 0 /usr/bin/hearing
7f1e00000000-7f1e01000000 r-xp 00000000 00:00 0 /lib/libonnxruntime.so.1.28.2
7f1e02000000-7f1e03000000 r-xp 00000000 00:00 0 /lib/libsherpa-onnx-c-api.so
";
    const WITH: &str = "\
7f1e00000000-7f1e01000000 r-xp 00000000 00:00 0 /lib/libonnxruntime.so.1.28.2
7f1e04000000-7f1e09000000 r-xp 00000000 00:00 0 /opt/lib/libonnxruntime_providers_cuda.so
";

    #[test]
    fn the_cuda_provider_is_found_in_the_mapped_libraries() {
        assert!(cuda_provider_is_mapped(WITH));
        assert!(!cuda_provider_is_mapped(WITHOUT));
        assert!(!cuda_provider_is_mapped(""));
    }

    #[test]
    fn a_cpu_model_is_always_where_it_should_be() {
        assert!(verify(Provider::Cpu).is_ok());
    }

    /// The fake `load` of the tests: fails for CUDA when `cuda_works` is false, and says what it
    /// was asked for.
    fn fake(
        cuda_works: bool,
        asked: &mut Vec<Provider>,
    ) -> impl FnMut(Provider) -> Result<&'static str> {
        move |provider| {
            asked.push(provider);
            match provider {
                Provider::Cuda if !cuda_works => anyhow::bail!("no CUDA here"),
                Provider::Cuda => Ok("on the GPU"),
                Provider::Cpu => Ok("on the CPU"),
            }
        }
    }

    #[test]
    fn a_working_gpu_is_used_without_any_fallback() {
        let mut asked = Vec::new();
        let (loaded, at) = place("test", Provider::Cuda, true, fake(true, &mut asked)).unwrap();
        assert_eq!((loaded, at), ("on the GPU", Provider::Cuda));
        assert_eq!(asked, [Provider::Cuda]);
    }

    #[test]
    fn a_missing_gpu_is_an_error_unless_the_fallback_is_asked_for() {
        let mut asked = Vec::new();
        let error = place("test", Provider::Cuda, false, fake(false, &mut asked)).unwrap_err();
        assert!(format!("{error:#}").contains("no CUDA here"));
        assert_eq!(asked, [Provider::Cuda], "the CPU was never tried");
    }

    #[test]
    fn a_missing_gpu_falls_back_to_the_cpu_when_asked_and_says_where_it_ended() {
        let mut asked = Vec::new();
        let (loaded, at) = place("test", Provider::Cuda, true, fake(false, &mut asked)).unwrap();
        assert_eq!((loaded, at), ("on the CPU", Provider::Cpu));
        assert_eq!(asked, [Provider::Cuda, Provider::Cpu]);
    }

    #[test]
    fn a_cpu_request_never_falls_back_and_fails_as_it_is() {
        let mut calls = 0;
        let error = place("test", Provider::Cpu, true, |_| -> Result<()> {
            calls += 1;
            anyhow::bail!("model missing")
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(format!("{error:#}").contains("model missing"));
    }

    #[test]
    fn a_failing_fallback_reports_both() {
        let error = place("test", Provider::Cuda, true, |_| -> Result<()> {
            anyhow::bail!("broken")
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("the CPU fallback failed too"));
    }
}
