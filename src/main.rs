#![feature(trim_prefix_suffix)]
#![feature(const_from)]
#![feature(const_trait_impl)]
#![allow(clippy::borrow_interior_mutable_const)]
#![allow(clippy::declare_interior_mutable_const)]
use nix::errno::Errno;
use nix::libc::{GLOB_NOSORT, glob, glob_t};
use nix::{
    sys::{
        statvfs::statvfs,
        sysinfo::{SysInfo, sysinfo},
        utsname::{UtsName, uname},
    },
    unistd::{Uid, User},
};

use byte_unit::UnitType;
use pci_info::PciInfo;
use pci_info::pci_enums::{PciDeviceClass, PciDeviceSubclass};
use xcb::randr::{GetScreenInfo, QueryVersion};
use xcb::x::{ATOM_WINDOW, Window};
use xcb::{Connection, x};

use core::error::Error;
use core::mem::MaybeUninit;
use std::cell::LazyCell;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::path::Path;

use procfs::{CpuInfo, Current as _, Meminfo};

const UNAME: LazyCell<UtsName> = LazyCell::new(|| {
    uname()
        .map_err(|_| Errno::last().desc())
        .expect("Uname failed")
});

#[expect(clippy::borrow_interior_mutable_const)]
const SYSINFO: LazyCell<SysInfo> = LazyCell::new(|| sysinfo().expect("Sysinfo failed"));

const RED: &str = "\x1b[1;31m";
const GREEN: &str = "\x1b[1;32m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[1;34m";
const PURPLE: &str = "\x1b[1;35m";
const CYAN: &str = "\x1b[1;36m";
#[expect(unused)]
const WHITE: &str = "\x1b[1;37m";
const CLEAR: &str = "\x1b[0m";

#[expect(clippy::borrow_interior_mutable_const)]
const COLOR: LazyCell<&str> = LazyCell::new(|| match DISTRO_INFO.0.as_str() {
    "gentoo" | "alpine" => PURPLE,
    "debian" => RED,
    "void" => "\x1b[1;38;5;34m",
    "ubuntu" => "\x1b[1;38;5;202m",
    "solus" => BLUE,
    "mint" | "opensuse" | "manjaro" => GREEN,
    "arch" | "artix" => CYAN,
    "freebsd" => RED,
    "openbsd" => YELLOW,
    "popos" | "pop_os" => "\x1b[1;38;5;29m",
    _ => RED,
});

macro_rules! color_print {
    ($($tok:tt)*) => {
        print!("{}", *COLOR);
        print!($($tok)*);
        print!("{}",CLEAR);
    }
}

macro_rules! color_println {
    (color=[$($color:tt)*], $($tok:tt)*) => {
        color_print!($($color)*);
        println!($($tok)*);
    };
}

#[expect(clippy::borrow_interior_mutable_const)]
const DISTRO_INFO: LazyCell<(String, String)> = LazyCell::new(|| {
    let file = File::open("/etc/os-release")
        .or_else(|_| File::open("/usr/lib/os-release"))
        .expect("failed to open os-release");

    let reader = BufReader::new(file);
    let (mut id, mut name) = ("Unknown".to_owned(), "Unknown".to_owned());

    let mut name_set = false;

    for line in reader.lines() {
        if line.is_err() {
            break;
        }
        let line = line.unwrap();
        if &line[0..3] == "ID=" {
            let rest = &line[3..];
            let is_quote = |c| c == '\'' || c == '\"';
            let no_quotes = rest.trim_start_matches(is_quote).trim_end_matches(is_quote);
            id = no_quotes.to_owned();
        } else if &line[0..12] == "PRETTY_NAME=" {
            let rest = &line[12..];
            let is_quote = |c| c == '\'' || c == '\"';
            let no_quotes = rest.trim_start_matches(is_quote).trim_end_matches(is_quote);
            name = no_quotes.to_owned();
            name_set = true;
        } else if &line[0..5] == "NAME=" && !name_set {
            let rest = &line[5..];
            let is_quote = |c| c == '\'' || c == '\"';
            let no_quotes = rest.trim_start_matches(is_quote).trim_end_matches(is_quote);
            name = no_quotes.to_owned();
        }
    }
    (id, name)
});

fn displays() -> Result<(), Box<dyn Error>> {
    let (c, _) = Connection::connect(None)?;
    for (n, x) in c.get_setup().roots().enumerate() {
        let ver = c.send_request(&QueryVersion {
            major_version: 1,
            minor_version: 1,
        });
        c.wait_for_reply(ver)?;

        let scrinfo = c.send_request(&GetScreenInfo { window: x.root() });
        let reply = c.wait_for_reply(scrinfo)?;

        color_println!(
            color = ["Screen {}:", n + 1],
            " {}x{} @ {}Hz",
            x.width_in_pixels(),
            x.height_in_pixels(),
            reply.rate()
        );
    }
    Ok(())
}

fn package_count() {
    let mut pkg_count = 0usize;
    let mut globbuf = MaybeUninit::<glob_t>::uninit();
    // SAFETY:
    //
    // There doesn't seem to be a safe wrapper for glob.
    unsafe {
        if glob(
            c"/var/db/pkg/*/*".as_ptr(),
            GLOB_NOSORT,
            None,
            globbuf.as_mut_ptr(),
        ) == 0
        {
            let globbuf = globbuf.assume_init();
            pkg_count = globbuf.gl_pathc;
        }
    }
    color_println!(color = ["Packages:"], " {}", pkg_count);
}

fn gpu() -> Result<(), Box<dyn Error>> {
    let info = PciInfo::enumerate_pci().expect("failed to enumerate PCI info");
    let mut gpu_names = Vec::<String>::with_capacity(1);
    let pci_ids = File::open("/usr/share/hwdata/pci.ids")
        .or_else(|_| File::open("/usr/share/pci.ids"))
        .or_else(|_| File::open("/usr/share/misc/pci.ids"))
        .or_else(|_| File::open("/var/lib/pciutils/pci.ids"))
        .or_else(|_| File::open("/usr/local/share/pciids/pci.ids"))
        .or_else(|_| File::open("/var/share/misc/pci_vendors"))?;
    for x in &info {
        match x {
            Ok(device)
                if device.device_class().unwrap() == PciDeviceClass::DisplayController
                    && device.device_subclass().unwrap()
                        == PciDeviceSubclass::DisplayController_VgaCompatible =>
            {
                let vendor_id = device.vendor_id();
                let vendor_string = format!("{vendor_id:X}");
                let reader = BufReader::new(&pci_ids);
                let mut it = reader.lines();
                while let Some(x) = it.next() {
                    if x.is_err() {
                        break;
                    }
                    let x = x.unwrap();
                    if x.starts_with(&vendor_string) {
                        let device_id = device.device_id();
                        let device_id_formatted_string = format!("\t{device_id:X}");
                        let x = it
                            .find(|line| {
                                line.as_ref()
                                    .unwrap()
                                    .starts_with(&device_id_formatted_string)
                            })
                            .unwrap_or_else(|| Ok("unknown GPU".to_owned()))?;
                        let gpu_name = x.trim_prefix(&device_id_formatted_string).trim().to_owned();

                        gpu_names.push(gpu_name);
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => println!("Error: {e}"),
        }
    }
    if gpu_names.len() == 1 {
        color_println!(color = ["GPU:"], " {}", gpu_names[0]);
    } else {
        for (num, gpu) in gpu_names.iter().enumerate() {
            color_println!(color = ["GPU {}:", num + 1], " {}", gpu);
        }
    }
    Ok(())
}

fn os_name() {
    let uname = &UNAME;
    let uname_os = uname.machine().to_str().unwrap();
    color_println!(color = ["OS:"], " {} {}", DISTRO_INFO.1, uname_os);
}
fn username_hostname() {
    let username = User::from_uid(Uid::effective())
        .expect("Getpwuid_r failed")
        .expect("Getting username failed")
        .name;
    let uname = &*UNAME;
    let hostname = uname.nodename().to_str().unwrap();
    let len = username.len() + 1 + hostname.len();
    color_print!("{}", username);
    print!("@");
    color_println!(color = ["{}", hostname],);
    color_println!(color = ["{}", "~".repeat(len)],);
}

fn kernel() {
    let uname = UNAME;
    let uname_kernel = uname.release().to_str().unwrap();
    color_println!(color = ["Kernel:"], " {}", uname_kernel);
}

fn shell() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let shell_path = Path::new(&shell)
        .file_name()
        .unwrap_or_else(|| OsStr::new("sh"));

    color_println!(color = ["Shell:"], " {}", shell_path.to_str().unwrap());
}

fn terminal() {
    let term = std::env::var("TERMINAL")
        .unwrap_or_else(|_| std::env::var("TERM").expect("Failed to get terminal info"));

    color_println!(color = ["Terminal:"], " {}", term);
}

fn uptime() -> Result<(), Box<dyn Error>> {
    let uptime = SYSINFO.uptime();
    let uptime = chrono::Duration::from_std(uptime)?;

    color_print!("Uptime: ");
    if uptime.num_days() > 0 {
        print!("{} days, ", uptime.num_days());
    }
    if uptime.num_hours() > 0 {
        print!("{} hours, ", uptime.num_hours() % 24);
    }
    println!("{} minutes", uptime.num_minutes() % 60);
    Ok(())
}

#[cfg(target_os = "linux")]
fn cpu() -> Result<(), Box<dyn Error>> {
    static TRIM_END_WORD_LIST: [&str; 2] = ["with Radeon Graphics", "Processor"];
    let cpuinfo = CpuInfo::current()?;
    let mut cpu_name = cpuinfo.model_name(0).ok_or("Failed to get cpu name")?;
    for end in TRIM_END_WORD_LIST {
        cpu_name = cpu_name.trim_end_matches(end);
    }
    cpu_name = cpu_name.trim_end();
    color_print!("CPU:");
    print!(" {} ({})", cpu_name, cpuinfo.num_cores());
    if let Some(s) = cpuinfo.get_field(0, "cpu MHz") {
        print!(" @ {s}MHz");
    }
    println!();
    Ok(())
}

#[cfg(target_os = "linux")]
fn memory() -> Result<(), Box<dyn Error>> {
    let meminfo = Meminfo::current()?;
    let total = meminfo.mem_total;
    let used = total
        - meminfo.mem_free
        - meminfo.buffers
        - meminfo.cached
        - meminfo.k_reclaimable.unwrap_or(0);
    let percent = used as f64 / total as f64 * 100.0;
    let used = byte_unit::Byte::from_u64(used).get_appropriate_unit(UnitType::Decimal);
    let total = byte_unit::Byte::from_u64(total).get_appropriate_unit(UnitType::Decimal);
    color_println!(
        color = ["Memory:"],
        " {:#.1} / {:#.1} ({:.1}%)",
        used,
        total,
        percent
    );
    Ok(())
}

fn disk() -> Result<(), Box<dyn Error>> {
    let statvfs = statvfs("/")?;
    let frsize = statvfs.fragment_size();
    let blocks = statvfs.blocks();
    let freeblks = statvfs.blocks_free();
    let total = blocks * frsize;
    let free = freeblks * frsize;
    let used = total - free;
    let percent = used as f64 / total as f64 * 100.0;
    let total = byte_unit::Byte::from_u64(total).get_appropriate_unit(UnitType::Decimal);
    let used = byte_unit::Byte::from_u64(used).get_appropriate_unit(UnitType::Decimal);

    color_println!(color = ["Disk:"], " {used:.1} / {total:.1} ({percent:.1}%)",);
    Ok(())
}

fn window_manager() -> Result<(), Box<dyn Error>> {
    let (c, _) = Connection::connect(None)?;
    let root_window = c
        .get_setup()
        .roots()
        .nth(0)
        .ok_or("Failed to get root window")?
        .root();
    xcb::atoms_struct! {
        pub(crate) struct Atoms {
            pub supporting_wm_check => b"_NET_SUPPORTING_WM_CHECK",
            pub net_wm_name => b"_NET_WM_NAME",
        }
    }
    let atoms = Atoms::intern_all(&c)?;
    let cookie = c.send_request(&x::GetProperty {
        delete: false,
        window: root_window,
        property: atoms.supporting_wm_check,
        r#type: x::ATOM_WINDOW,
        long_offset: 0,
        long_length: 1024,
    });
    let reply = c.wait_for_reply(cookie)?;
    assert!(reply.r#type() == ATOM_WINDOW);
    let list = reply.value::<Window>();
    let window = list[0];
    let cookie = c.send_request(&x::GetProperty {
        delete: false,
        window,
        property: atoms.net_wm_name,
        r#type: x::ATOM_ANY,
        long_offset: 0,
        long_length: 1024,
    });
    let reply = c.wait_for_reply(cookie)?;
    let wm_name = str::from_utf8(reply.value::<u8>())?;
    color_println!(color = ["WM:"], " {}", wm_name);

    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    username_hostname();
    os_name();
    kernel();
    uptime()?;
    package_count();
    shell();
    displays()?;
    window_manager()?;
    terminal();
    cpu()?;
    gpu()?;
    memory()?;
    disk()?;
    Ok(())
}
