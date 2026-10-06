#[cfg(windows)]
use windows::Win32::Foundation::GetLastError;
#[cfg(windows)]
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};

#[derive(Debug, Clone)]
pub struct ProcessorTopology {
    pub p_cores: Vec<usize>,
    pub e_cores: Vec<usize>,
}

pub fn get_processor_topology() -> ProcessorTopology {
    #[cfg(windows)]
    {
        get_windows_topology()
    }
    #[cfg(not(windows))]
    {
        ProcessorTopology {
            p_cores: Vec::new(),
            e_cores: Vec::new(),
        }
    }
}

#[cfg(windows)]
fn get_windows_topology() -> ProcessorTopology {
    let mut buffer_size: u32 = 0;

    // First call to get buffer size
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut buffer_size);
    }

    let last_error = unsafe { GetLastError() };
    if buffer_size == 0 {
        eprintln!(
            "Failed to get processor info buffer size. Error: {:?}",
            last_error
        );
        return ProcessorTopology {
            p_cores: Vec::new(),
            e_cores: Vec::new(),
        };
    }

    let mut buffer: Vec<u8> = vec![0; buffer_size as usize];

    let result = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buffer.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut buffer_size,
        )
    };

    if result.is_err() {
        eprintln!("Failed to get processor info. Error: {:?}", unsafe {
            GetLastError()
        });
        return ProcessorTopology {
            p_cores: Vec::new(),
            e_cores: Vec::new(),
        };
    }

    let mut p_cores = Vec::new();
    let mut e_cores = Vec::new();

    // Re-loop with cleaner parsing logic
    let mut items = Vec::new();
    let mut offset = 0;
    while offset < buffer_size as usize {
        let entry = unsafe {
            &*(buffer.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };

        if entry.Relationship == RelationProcessorCore {
            let core_info = unsafe { &entry.Anonymous.Processor };
            let efficiency_class = core_info.EfficiencyClass;

            for i in 0..core_info.GroupCount {
                let group_mask = unsafe { core_info.GroupMask.get_unchecked(i as usize) };
                let mask = group_mask.Mask;
                for bit in 0..64 {
                    if (mask >> bit) & 1 == 1 {
                        let logical_mask = 1usize << bit;
                        items.push((efficiency_class, logical_mask));
                    }
                }
            }
        }
        offset += entry.Size as usize;
    }

    let max_efficiency = items.iter().map(|(c, _)| *c).max().unwrap_or(0);
    let min_efficiency = items.iter().map(|(c, _)| *c).min().unwrap_or(0);

    if max_efficiency == min_efficiency {
        // Uniform architecture or detection failed. Treat all as P-cores (default pool).
        for (_, mask) in items {
            p_cores.push(mask);
        }
        // No E-cores
    } else {
        // Hybrid
        for (class, mask) in items {
            if class == max_efficiency {
                p_cores.push(mask);
            } else {
                e_cores.push(mask);
            }
        }
    }

    // If no E-cores found (but we had different classes? Unlikely logic above covers it),
    // e_cores is empty.

    ProcessorTopology { p_cores, e_cores }
}

pub fn bind_thread_to_core(mask: usize) {
    #[cfg(windows)]
    unsafe {
        let _ = SetThreadAffinityMask(GetCurrentThread(), mask);
    }
}

/// Physical memory currently available to this process, in bytes.
///
/// Returns `None` when it cannot be determined, in which case callers must fall back to a
/// CPU-only decision rather than guessing a number.
#[cfg(windows)]
pub fn available_memory_bytes() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some(status.ullAvailPhys)
}

#[cfg(not(windows))]
pub fn available_memory_bytes() -> Option<u64> {
    // Linux/macOS detection not implemented; callers fall back to a CPU-only decision.
    None
}

/// Total physical memory, in bytes. `None` when it cannot be determined.
#[cfg(windows)]
pub fn total_memory_bytes() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some(status.ullTotalPhys)
}

#[cfg(not(windows))]
pub fn total_memory_bytes() -> Option<u64> {
    None
}
