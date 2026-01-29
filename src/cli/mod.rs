pub mod app_window;
pub mod generate_svg;
pub mod wgpu_window;

use crate::cli::app_window::spawn_window;
use crate::cli::generate_svg::generate_svg;
use crate::cli::wgpu_window::spawn_wgpu_window;
use crate::core::app_controller::Theme;
use crate::core::instancer::Instancer;
use crate::core::loader::Loader;
use crate::core::root_finder::RootFinder;

use anyhow::anyhow;
use anyhow::Result;
use clap::Parser;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Input GDSII file to process
    #[arg(required = true)]
    pub input: PathBuf,

    /// Optional output SVG file to generate
    #[arg(value_name = "OUTPUT.svg")]
    pub output: Option<PathBuf>,

    /// Request OpenGL window with interactive visualization
    #[arg(long)]
    pub gl: bool,

    /// Request wgpu window (skeleton backend; currently clears only)
    #[arg(long)]
    pub wgpu: bool,

    /// Use light theme instead of dark theme
    #[arg(long)]
    pub light: bool,
}

fn verify_file_extension(path: &Path, expected: &str) -> Result<()> {
    match path.extension() {
        Some(ext) if ext.to_string_lossy() == expected => Ok(()),
        _ => Err(anyhow!(
            "File '{}' must have .{} extension",
            path.display(),
            expected
        )),
    }
}

pub fn run_cli() -> Result<()> {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();

    // Verify file extensions
    verify_file_extension(&args.input, "gds")?;
    if let Some(ref output_path) = args.output {
        verify_file_extension(output_path, "svg")?;
    }

    println!(
        "Reading {}...",
        args.input.file_name().unwrap().to_string_lossy()
    );

    // Read and process the GDSII file
    let file_content = fs::read(&args.input)?;

    let mut world = pollster::block_on(async {
        let loader = Loader::new(&file_content);
        let mut world = None;
        for mut progress in loader {
            print!(".");
            world = progress.take_world();
        }
        let mut world = world.expect("World was not yielded");
        log::info!("Done with loading.");

        let mut root_finder = RootFinder::new(&mut world);
        let roots = root_finder.find_roots(&world);

        log::info!("Found {} roots.", roots.len());

        let mut instancer = Instancer::new(&mut world);
        instancer.select_root(&mut world, roots[0]);

        log::info!("Done with instantiation.");

        world
    });

    // Generate and save SVG if output path is provided
    if let Some(ref output_path) = args.output {
        let svg_content = generate_svg(&mut world);

        fs::write(output_path, svg_content)?;
        println!("SVG file written to: {}", output_path.display());
    }

    println!();

    let theme = if args.light { Theme::Light } else { Theme::Dark };

    if args.wgpu {
        spawn_wgpu_window(world, theme)?;
    } else if args.gl {
        spawn_window(world, theme)?;
    }

    Ok(())
}
