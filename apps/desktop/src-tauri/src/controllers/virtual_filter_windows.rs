use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Device_Interface_PropertyW, CM_Get_Parent, CM_Locate_DevNodeW,
    CR_SUCCESS,
};
use windows_sys::Win32::Devices::Properties::{DEVPKEY_Device_InstanceId, DEVPROP_TYPE_STRING};
pub fn virtual_bus(path: &str) -> bool {
    if !path.starts_with(r"\\?\") || path.contains('\0') || path.len() > 4096 {
        return false;
    }
    let path: Vec<_> = path.encode_utf16().chain([0]).collect();
    let mut instance = [0u16; 512];
    let mut bytes = std::mem::size_of_val(&instance) as u32;
    let mut kind = 0;
    let mut node = 0;
    #[allow(unsafe_code)]
    // SAFETY: terminated interface path and fixed initialized writable
    // buffers; the API receives their exact byte sizes. No handles escape.
    unsafe {
        if CM_Get_Device_Interface_PropertyW(
            path.as_ptr(),
            &DEVPKEY_Device_InstanceId,
            &mut kind,
            instance.as_mut_ptr().cast(),
            &mut bytes,
            0,
        ) != CR_SUCCESS
            || kind != DEVPROP_TYPE_STRING
            || !instance.contains(&0)
            || CM_Locate_DevNodeW(&mut node, instance.as_mut_ptr(), 0) != CR_SUCCESS
        {
            return false;
        }
    }
    // Native HID device -> USB child -> virtual bus; bound malformed trees.
    for _ in 0..8 {
        let mut name = [0u16; 512];
        #[allow(unsafe_code)]
        // SAFETY: Configuration Manager's device instance is obtained above;
        // name has the declared UTF-16 capacity and is inspected only on success.
        let status = unsafe { CM_Get_Device_IDW(node, name.as_mut_ptr(), name.len() as u32, 0) };
        if status != CR_SUCCESS {
            return false;
        }
        let Some(end) = name.iter().position(|unit| *unit == 0) else {
            return false;
        };
        let Some(name) = name.get(..end) else {
            return false;
        };
        if String::from_utf16_lossy(name)
            .to_ascii_uppercase()
            .starts_with("ROOT\\VIGEMBUS\\")
        {
            return true;
        }
        let mut parent = 0;
        #[allow(unsafe_code)]
        // SAFETY: live device instance and initialized output slot, used on success.
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            return false;
        }
        node = parent;
    }
    false
}
