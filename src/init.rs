use crate::{
    chip::{
        family::{mem_region::MemRegion, Family},
        target::Target,
        Chip,
    },
    error::{Error, InvalidChip},
    parser::init_args::{panic_handler::PanicHandler, soft_device::Softdevice, InitArgs},
};
use indicatif::ProgressBar;
use inflector::cases::snakecase::to_snake_case;
use probe_rs::config::{get_target_by_name, search_chips};
use std::{
    env::set_current_dir,
    fs,
    io::{Read, Write},
    process::Command,
    time::Duration,
};

pub struct Init {
    pb: ProgressBar,
}

impl Init {
    pub fn new() -> Self {
        let pb = ProgressBar::new_spinner();
        pb.enable_steady_tick(Duration::from_millis(100));

        Self { pb }
    }

    pub fn run(&self, args: InitArgs) {
        if let Err(e) = self.run_inner(args) {
            self.pb
                .abandon_with_message(format!("Failed with error: {e:#?}."))
        } else {
            self.pb
                .finish_with_message(format!("Finished in {}s", self.pb.elapsed().as_secs()))
        }
    }

    fn run_inner(&self, mut args: InitArgs) -> Result<(), Error> {
        // for convenience
        args.chip_name = args.chip_name.replace('-', "_").to_lowercase();

        let (chip, probe_target_name) = self.get_target_info(&args.chip_name)?;

        // validate softdevice <--> nrf
        if args.softdevice.is_some() && !matches!(chip.family, Family::NRF(_)) {
            return Err(Error::ErroneousSoftdevice);
        }

        self.create_project(&args.name)?;

        self.init_config(&chip.target, &probe_target_name)?;
        self.init_toolchain(&chip.target)?;
        self.init_embed(&probe_target_name)?;
        self.init_build(&chip.family)?;
        self.init_manifest(
            &args.name,
            &chip,
            &args.panic_handler,
            args.softdevice.as_ref(),
        )?;
        self.init_fmt()?;
        self.init_main(&chip.family, &args.panic_handler, args.softdevice.as_ref())?;

        if let Family::NRF(mem_reg) = chip.family {
            self.init_memory_x(mem_reg)?;
            self.pb.println("[ACTION NEEDED] You must now flash the Softdevice and configure memory.x. Instructions can be found here: https://github.com/embassy-rs/nrf-softdevice#running-examples.");
        }

        Ok(())
    }

    fn create_project(&self, name: &str) -> Result<(), Error> {
        self.pb.set_message("Create cargo project");
        Command::new("cargo")
            .args(["new", &name])
            .output()
            .map_err(|_| Error::CreateCargo)?;

        set_current_dir(name).map_err(|_| Error::ChangeDir)
    }

    fn get_target_info(&self, name: &str) -> Result<(Chip, String), Error> {
        self.pb.set_message("Searching chips");
        if let Ok(chips) = search_chips(name) {
            let probe_target = get_target_by_name(
                chips
                    .first()
                    .ok_or(Error::InvalidChip(InvalidChip::Unknown))?,
            )
            .unwrap();

            Ok((name.parse()?, probe_target.name))
        } else {
            Err(Error::InvalidChip(InvalidChip::Unknown))
        }
    }

    fn init_config(&self, target: &Target, chip: &str) -> Result<(), Error> {
        fs::create_dir_all(".cargo").map_err(|_| Error::CreateFolder(".cargo".into()))?;

        self.create_file(
            ".cargo/config.toml",
            &format!(
                include_str!("templates/config.toml.template"),
                target = target,
                chip = chip
            ),
        )
    }

    fn init_toolchain(&self, target: &Target) -> Result<(), Error> {
        self.create_file(
            "rust-toolchain.toml",
            &format!(
                include_str!("templates/rust-toolchain.toml.template"),
                target = target
            ),
        )
    }

    fn init_embed(&self, chip: &str) -> Result<(), Error> {
        self.create_file(
            "Embed.toml",
            &format!(include_str!("templates/Embed.toml.template"), chip = chip),
        )
    }

    fn init_build(&self, family: &Family) -> Result<(), Error> {
        let template = match family {
            Family::STM32 => include_str!("templates/build.rs.stm32.template"),
            Family::NRF(_) => include_str!("templates/build.rs.nrf.template"),
        };

        self.create_file("build.rs", template)
    }

    fn init_manifest(
        &self,
        name: &str,
        chip: &Chip,
        panic_handler: &PanicHandler,
        softdevice: Option<&Softdevice>,
    ) -> Result<(), Error> {
        self.create_file(
            "Cargo.toml",
            &format!(include_str!("templates/Cargo.toml.template"), name = name),
        )?;

        // NOTE: should be threaded proably
        self.cargo_add(
            "embassy-executor",
            Some(&["arch-cortex-m", "executor-thread", "integrated-timers"]),
            false,
        )?;
        self.cargo_add("embassy-sync", None, false)?;
        self.cargo_add("embassy-futures", None, false)?;
        self.cargo_add("embassy-time", Some(&["tick-hz-32_768"]), false)?;

        match chip.family {
            Family::STM32 => self.cargo_add(
                "embassy-stm32",
                Some(&[
                    "memory-x",
                    chip.name.as_str(),
                    "time-driver-any",
                    "exti",
                    "unstable-pac",
                ]),
                false,
            ),
            Family::NRF(_) => self.cargo_add(
                "embassy-nrf",
                Some(&[chip.name.as_str(), "gpiote", "time-driver-rtc1"]),
                false,
            ),
        }?;

        if let Some(softdevice) = softdevice {
            self.cargo_add(
                "nrf-softdevice",
                Some(&[
                    chip.name.as_str(),
                    softdevice.str(),
                    "ble-peripheral",
                    "ble-gatt-server",
                    "critical-section-impl",
                ]),
                false,
            )?;
            self.cargo_add(&format!("nrf-softdevice-{}", softdevice.str()), None, false)?;
        }

        self.cargo_add(
            "cortex-m",
            Some(if softdevice.is_some() {
                &["inline-asm"]
            } else {
                &["inline-asm", "critical-section-single-core"]
            }),
            false,
        )?;
        self.cargo_add("cortex-m-rt", None, false)?;
        self.cargo_add("defmt", None, true)?;
        self.cargo_add("defmt-rtt", None, true)?;
        self.cargo_add("panic-probe", Some(&["print-defmt"]), true)?;
        self.cargo_add(panic_handler.str(), None, false)?;

        let mut file = fs::OpenOptions::new()
            .read(true)
            .append(true)
            .open("Cargo.toml")
            .map_err(|_| Error::CreateFile("Cargo.toml".into()))?;

        // really gross patch for cargo version discontinuity
        // somewhere between cargo 1.72 and 1.76 the behavior of "cargo add" changed
        let mut buf = String::new();
        file.read_to_string(&mut buf).unwrap();
        if !buf.contains("[features]") {
            file.write_all(include_str!("templates/Cargo.toml.feature-patch.template").as_bytes())
                .map_err(|_| Error::CreateFile("Cargo.toml".into()))?;
        }

        file.write_all(
            if softdevice.is_some() {
                include_str!("templates/Cargo.toml.sd.append").into()
            } else {
                format!(
                    include_str!("templates/Cargo.toml.append"),
                    family = chip.family
                )
            }
            .as_bytes(),
        )
        .map_err(|_| Error::CreateFile("Cargo.toml".into()))?;

        Ok(())
    }

    fn init_fmt(&self) -> Result<(), Error> {
        self.create_file("src/fmt.rs", include_str!("templates/fmt.rs.template"))
    }

    fn init_main(
        &self,
        family: &Family,
        panic_handler: &PanicHandler,
        softdevice: Option<&Softdevice>,
    ) -> Result<(), Error> {
        let panic_handler = to_snake_case(panic_handler.str());

        self.create_file(
            "src/main.rs",
            &match (family, softdevice) {
                (Family::STM32, _) => format!(
                    include_str!("templates/main.rs.stm32.template"),
                    panic_handler = panic_handler
                ),
                (Family::NRF(_), Some(_)) => {
                    format!(
                        include_str!("templates/main.rs.nrf.sd.template"),
                        panic_handler = panic_handler
                    )
                }
                (Family::NRF(_), None) => {
                    format!(
                        include_str!("templates/main.rs.nrf.template"),
                        panic_handler = panic_handler
                    )
                }
            },
        )
    }

    fn init_memory_x(&self, memory: MemRegion) -> Result<(), Error> {
        self.create_file(
            "memory.x",
            &format!(
                include_str!("templates/memory.x.template"),
                flash_origin = memory.flash_origin,
                flash_len = memory.flash_length,
                ram_origin = memory.ram_origin,
                ram_len = memory.ram_length,
            ),
        )
    }

    fn create_file(&self, name: &str, content: &str) -> Result<(), Error> {
        self.pb.set_message(format!("Create file: {name}"));

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(name)
            .map_err(|_| Error::CreateFile(name.into()))?;

        file.write_all(content.as_bytes())
            .map_err(|_| Error::CreateFile(name.into()))?;

        Ok(())
    }

    fn cargo_add(
        &self,
        name: &str,
        features: Option<&[&str]>,
        optional: bool,
    ) -> Result<(), Error> {
        self.pb.set_message(format!("Cargo add: {name}"));

        let features = features.unwrap_or_default().join(",");
        let mut cmd = Command::new("cargo");

        cmd.arg("add")
            .args([name, &format!("--features={features}")]);

        if optional {
            cmd.arg("--optional");
        }

        cmd.output().map_err(|_| Error::CargoAdd(name.into()))?;

        Ok(())
    }
}
