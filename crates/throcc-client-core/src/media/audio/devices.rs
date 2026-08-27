use cpal::traits::{DeviceTrait as _, HostTrait as _};

use crate::{Error, Result};

/// A device as the interface above renders it. No `cpal` type crosses this
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn inputs() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(identifier);
    Ok(listed(&host, default.as_deref())
        .into_iter()
        .filter(|(device, _)| device.supports_input())
        .map(|(_, described)| described)
        .collect())
}

pub fn outputs() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(identifier);
    Ok(listed(&host, default.as_deref())
        .into_iter()
        .filter(|(device, _)| device.supports_output())
        .map(|(_, described)| described)
        .collect())
}

/// The device carrying that id, or the host's default when the id is unknown, so
/// a device that was unplugged since it was chosen does not stop a call.
pub fn input(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    matching(&host, id)
        .filter(cpal::Device::supports_input)
        .or_else(|| host.default_input_device())
        .ok_or_else(|| Error::Audio("this machine has no audio input device".into()))
}

pub fn output(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    matching(&host, id)
        .filter(cpal::Device::supports_output)
        .or_else(|| host.default_output_device())
        .ok_or_else(|| Error::Audio("this machine has no audio output device".into()))
}

fn matching(host: &cpal::Host, id: Option<&str>) -> Option<cpal::Device> {
    let wanted = id?;
    host.devices()
        .ok()?
        .find(|device| identifier(device.clone()).is_some_and(|found| found == wanted))
}

fn listed(host: &cpal::Host, default: Option<&str>) -> Vec<(cpal::Device, AudioDevice)> {
    let Ok(devices) = host.devices() else {
        tracing::warn!("the audio host lists no devices");
        return Vec::new();
    };

    devices
        .filter_map(|device| {
            let id = identifier(device.clone())?;
            let name = match device.description() {
                Ok(description) => description.name().to_string(),
                Err(e) => {
                    tracing::debug!(error = %e, %id, "skipping a device that will not describe itself");
                    return None;
                }
            };
            let described = AudioDevice {
                is_default: default == Some(id.as_str()),
                id,
                name,
            };
            Some((device, described))
        })
        .collect()
}

fn identifier(device: cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_answers_on_whatever_this_machine_has() {
        let inputs = inputs().expect("enumerating inputs");
        let outputs = outputs().expect("enumerating outputs");
        for device in inputs.iter().chain(&outputs) {
            assert!(!device.id.is_empty(), "a device needs an id to be chosen");
        }
        assert!(
            inputs.iter().filter(|device| device.is_default).count() <= 1,
            "at most one input is the default"
        );
        assert!(
            outputs.iter().filter(|device| device.is_default).count() <= 1,
            "at most one output is the default"
        );
    }
}
