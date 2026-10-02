use super::XState;
use log::warn;
use xcb::x;

/// The logical cursor size: our own XCURSOR_SIZE (the compositor's, when it
/// spawns us), or 24.
pub(super) fn logical_cursor_size() -> u32 {
    std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|size| size.parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(24)
}

impl XState {
    /// Sets the resources that follow the scale X clients render at. `Xcursor.size`
    /// matters because clients render in physical pixels here and pick their
    /// cursor images by size: at the logical size they would come out half as
    /// big, or be upscaled by the compositor. A client's own XCURSOR_SIZE still
    /// wins over the resource.
    pub(super) fn update_scaled_resources(&self, dpi: i32, cursor_size: u32) {
        // Other clients may replace the resource database so don't cache its contents
        let reply = self
            .connection
            .wait_for_reply(self.connection.send_request(&x::GetProperty {
                delete: false,
                window: self.root,
                property: self.atoms.resource_manager,
                r#type: x::ATOM_STRING,
                long_offset: 0,
                long_length: u32::MAX,
            }))
            .unwrap();

        let resources = match reply.r#type() {
            x::ATOM_NONE => &[],
            x::ATOM_STRING => reply.value::<u8>(),
            other => {
                warn!("RESOURCE_MANAGER has unexpected type {other:?}");
                return;
            }
        };

        let scaled: [(&[u8], String); 2] = [
            (b"Xft.dpi", format!("Xft.dpi:\t{dpi}")),
            (b"Xcursor.size", format!("Xcursor.size:\t{cursor_size}")),
        ];
        let mut updated = Vec::with_capacity(resources.len() + 64);
        let mut replaced = [false; 2];

        for line in resources.split_inclusive(|byte| *byte == b'\n') {
            let resource_name = line
                .iter()
                .position(|byte| *byte == b':')
                .map(|separator| line[..separator].trim_ascii());
            match scaled
                .iter()
                .position(|(name, _)| resource_name == Some(*name))
            {
                Some(i) => {
                    if !replaced[i] {
                        updated.extend_from_slice(scaled[i].1.as_bytes());
                        if line.ends_with(b"\n") {
                            updated.push(b'\n');
                        }
                        replaced[i] = true;
                    }
                }
                None => updated.extend_from_slice(line),
            }
        }

        for (i, (_, resource)) in scaled.iter().enumerate() {
            if !replaced[i] {
                if !updated.is_empty() && !updated.ends_with(b"\n") {
                    updated.push(b'\n');
                }
                updated.extend_from_slice(resource.as_bytes());
                updated.push(b'\n');
            }
        }

        self.connection
            .send_and_check_request(&x::ChangeProperty {
                window: self.root,
                mode: x::PropMode::Replace,
                property: self.atoms.resource_manager,
                r#type: x::ATOM_STRING,
                data: &updated,
            })
            .unwrap();
    }
}
