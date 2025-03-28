#[macro_use] extern crate log;
#[macro_use] extern crate onetagger_shared;

use anyhow::Error;
use onetagger_ui::StartContext;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use clap::{Parser, Subcommand};
use convert_case::{Casing, Case};
use onetagger_platforms::spotify::Spotify;
use onetagger_renamer::{RenamerConfig, Renamer, TemplateParser};
use onetagger_shared::{VERSION, COMMIT};
use onetagger_autotag::audiofeatures::{AudioFeaturesConfig, AudioFeatures};
use onetagger_autotag::{Tagger, TaggerConfigExt, AudioFileInfoImpl};
use onetagger_tagger::{TaggerConfig, AudioFileInfo, SupportedTag};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Default configs
    if cli.autotagger_config {
        let config = serde_json::to_string_pretty(&TaggerConfig::custom_default()).expect("Failed serializing default config!");
        println!("{config}");
        return Ok(());
    }
    if cli.audiofeatures_config {
        let config = serde_json::to_string_pretty(&AudioFeaturesConfig::default()).expect("Failed serializing config!");
        println!("{config}");
        return Ok(());
    }

    if cli.action.is_none() {
        println!("No action. Use onetagger-cli --help to get print help.");
        return Ok(());
    }

    // Setup logging
    onetagger_shared::setup();
    info!("\n\nStarting OneTagger v{VERSION} Commit: {COMMIT} OS: {}\n\n", std::env::consts::OS);


    let action = cli.action.unwrap();
    match &action {
        Actions::Autotagger { path, .. } => {
            let config = action.get_at_config().expect("Failed loading config file!");
            debug!("{:?}", config);

            // Get files
            let files = if path.is_file() {
                onetagger_playlist::get_files_from_playlist_file(path).expect("Not a valid playlist file")
            } else {
                AudioFileInfo::get_file_list(&path, config.include_subfolders)
            };

            let rx = Tagger::tag_files(&config, files, Arc::new(Mutex::new(None)));
            let start = timestamp!();
            for status in rx {
                debug!("{status:?}");
            }
            info!("Tagging finished, took: {} seconds.", (timestamp!() - start) / 1000);
        },
        Actions::Audiofeatures { path, config, client_id, client_secret, no_subfolders } => {
            let file = File::open(config).expect("Failed reading config file!");
            let config: AudioFeaturesConfig = serde_json::from_reader(&file).expect("Failed parsing config file!");
            // Cli subfolders override
            let mut subfolders = config.include_subfolders;
            if *no_subfolders {
                subfolders = false;
            }
            // Auth spotify
            let spotify = Spotify::try_cached_token(client_id, client_secret)
                .expect("Spotify unauthorized, please run the authorize-spotify option or login to Spotify in UI at least once!");

            // Get files
            let files = if path.is_file() {
                onetagger_playlist::get_files_from_playlist_file(path).expect("Not a valid playlist file")
            } else {
                AudioFileInfo::get_file_list(&path, subfolders)
            };

            let rx = AudioFeatures::start_tagging(config, spotify, files);
            let start = timestamp!();
            for status in rx {
                debug!("{status:?}");
            }
            info!("Tagging finished, took: {} seconds.", (timestamp!() - start) / 1000);
        },
        Actions::QueryUrl { url, confidence } => {
            info!("Querying URL: {} with confidence: {}", url, confidence);
            
            match onetagger_songdownloader::get_url_info_with_confidence(url, *confidence) {
                Ok(info) => {
                    println!("\nURL Information:");
                    println!("Platform:     {}", info.platform);
                    println!("Content Type: {}", info.content_type);
                    println!("Title:        {}", info.title);
                    if let Some(desc) = info.description {
                        println!("Description:  {}", desc);
                    }
                    
                    // Display video tracklists if available
                    if let Some(video_tracklists) = info.video_tracklists {
                        println!("\nExtracted Tracklists:");
                        let mut json_output = serde_json::Map::new();
                        
                        // Add the channel name
                        json_output.insert("Youtube Channel".to_string(), serde_json::Value::String(info.title.clone()));
                        
                        // Add each video's tracklist
                        for (video_title, tracklist) in video_tracklists {
                            json_output.insert(video_title, serde_json::Value::Array(
                                tracklist.into_iter().map(|s| serde_json::Value::String(s)).collect()
                            ));
                        }
                        
                        println!("{}", serde_json::to_string_pretty(&json_output).unwrap());
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Failed to get URL information: {}", e).into());
                }
            }
        },
        Actions::SongDownloader { url, output, confidence, enable_auto_tag, auto_tag_config, enable_audio_features, client_id, client_secret } => {
            info!("Starting song downloader for URL: {}", url);
            
            // Get the path to the Python script
            let script_path = std::env::current_dir()?
                .join("YoutubeToSpotify")
                .join("downloader.py");
            
            // Check if the script exists
            if !script_path.exists() {
                return Err(anyhow::anyhow!("Song downloader script not found at {:?}", script_path).into());
            }
            
            // Create the output directory if it doesn't exist
            std::fs::create_dir_all(output)?;
            
            // Build the command
            let mut cmd = std::process::Command::new("python");
            cmd.arg(&script_path)
                .arg("--url").arg(url)
                .arg("--output").arg(output)
                .arg("--confidence").arg(confidence.to_string());
            
            // Add optional flags
            if *enable_auto_tag {
                cmd.arg("--enable-auto-tag");
                if let Some(config) = auto_tag_config {
                    cmd.arg("--auto-tag-config").arg(config);
                }
            }
            
            if *enable_audio_features {
                cmd.arg("--enable-audio-features");
                if let (Some(id), Some(secret)) = (client_id, client_secret) {
                    cmd.arg("--client-id").arg(id)
                       .arg("--client-secret").arg(secret);
                } else {
                    return Err(anyhow::anyhow!("Spotify client ID and secret are required for audio features").into());
                }
            }
            
            // Run the command
            let output = cmd.output()?;
            
            if output.status.success() {
                info!("Songs downloaded successfully!");
                println!("{}", String::from_utf8_lossy(&output.stdout));
            } else {
                error!("Failed to download songs: {}", String::from_utf8_lossy(&output.stderr));
                return Err(anyhow::anyhow!("Failed to download songs").into());
            }
        },
        // Spotify OAuth flow
        Actions::AuthorizeSpotify { client_id, client_secret, prompt, expose } => {
            let (auth_url, client) = Spotify::generate_auth_url(&client_id, &client_secret).expect("Failed generating auth URL!");
            println!("\nPlease go to the following URL and authorize 1T:\n{auth_url}");
            // should cache the token
            match prompt {
                true => {
                    println!("\nEnter the URL you were redirected to and press enter: ");
                    let mut url = String::new();
                    std::io::stdin().read_line(&mut url).expect("Couldn't read from stdin!");
                    let _spotify = Spotify::auth_token_code(client, url.trim()).expect("Spotify authentication failed!");
                },
                false => {
                    let expose = *expose;
                    std::thread::spawn(move || {
                        onetagger_ui::start_all(StartContext {
                            server_mode: true,
                            start_path: None,
                            expose,
                            browser: false,
                        }).expect("Failed starting server!");
                    });
                    let _spotify = Spotify::auth_server(client).expect("Spotify authentication failed!");
                }
            }
            info!("Succesfully authorized Spotify!");
            // Exit because of webserver
            std::process::exit(0);
        },
        // Renamer
        Actions::Renamer { path, output, template, copy, no_subfolders, preview, overwrite, separator, keep_subfolders } => {
            let config = RenamerConfig {
                path: path.to_owned(),
                out_dir: output.to_owned(),
                template: template.to_string(),
                copy: *copy,
                subfolders: !*no_subfolders,
                overwrite: *overwrite,
                separator: separator.to_string(),
                keep_subfolders: *keep_subfolders,
            };
            let mut renamer = Renamer::new(TemplateParser::parse(&template));
            let files = AudioFileInfo::load_files_iter(&config.path, config.subfolders, None, None);
            let names = renamer.generate(files, &config).expect("Failed generating filenames!");

            // Only preview
            if *preview {
                for (i, (from, to)) in names.iter().enumerate() {
                    println!("{}. {:?} -> {:?}", i + 1, from, to);
                }
                return Ok(());
            }

            renamer.rename(&names, &config).expect("Failed renaming!");
        },
        // Server mode
        Actions::Server { expose, path, browser } => {
            onetagger_ui::start_all(StartContext {
                server_mode: true,
                start_path: path.clone().map(String::from),
                expose: *expose,
                browser: *browser,
            }).expect("Failed starting the server");
        }
    }
    
    Ok(())
}


#[derive(Parser, Debug, Clone)]
#[clap(version)]
struct Cli {
    /// What should OneTagger do
    #[clap(subcommand)]
    action: Option<Actions>,
    
    /// Prints the default Autotagger config and exits
    #[clap(long)]
    autotagger_config: bool,

    /// Prints the default Audio Features config and exits
    #[clap(long)]
    audiofeatures_config: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Actions {
    /// Start Autotagger in CLI mode
    Autotagger {
        /// Path to music files (overrides config)
        #[clap(short, long)]
        path: PathBuf,

        /// Specify a path to config file
        #[clap(short, long)]
        config: Option<PathBuf>,

        /// Comma separated list of platforms to use. For custom platforms use the library filename
        #[clap(short = 'P', long)]
        platforms: Option<String>,

        /// Comma separated list of tags to use
        #[clap(short, long)]
        tags: Option<String>,

        /// Use ID3v2.4 instead of IDv2.3 for MP3/AIFF files
        #[clap(long)]
        id3v24: bool,

        /// Overwrite the existing tags in the track
        #[clap(long)]
        overwrite: bool,

        /// How many threads to use for the searching & matching process
        #[clap(long)]
        threads: Option<u16>,

        /// How strict should the matching be? Use: 0 - 100, Default: 80 (%).
        #[clap(long)]
        strictness: Option<u8>,

        /// Writes a cover.jpg into the folder
        #[clap(long)]
        album_art_file: bool,

        /// Merge new genres with existing ones
        #[clap(long)]
        merge_genres: bool,

        /// Write the key tag in CAMELOT format
        #[clap(long)]
        camelot: bool,

        /// Write title tag without version (ie. remix)
        #[clap(long)]
        short_title: bool,

        /// Match the song duration as well (WARNING: very strict)
        #[clap(long)]
        match_duration: bool,

        /// If duration matching is enabled, how big the difference in durations can be (in seconds)
        #[clap(long)]
        max_duration_difference: Option<u64>,

        /// Use platform specific ID tags to get exact matches
        #[clap(long)]
        match_by_id: bool,

        /// Try to indentify the track on Shazam if title & artist tags are missing
        #[clap(long)]
        enable_shazam: bool,

        /// Always try to indentify the track on Shazam
        #[clap(long)]
        force_shazam: bool,

        /// Skip tracks that have 1T_TAGGEDDATE tag
        #[clap(long)]
        skip_tagged: bool,

        /// Try to get title & artist from filename if the tags are missing
        #[clap(long)]
        parse_filename: bool,

        /// Template for parse_filename option. Example: `%track$. %artists% - %title%`
        #[clap(long)]
        filename_template: Option<String>,

        /// Don't include subfolders
        #[clap(long)]
        no_subfolders: bool,

        /// Write only year instead of full date
        #[clap(long)]
        only_year: bool,

        /// Tag on multiple platforms instead of the default fallback mode
        #[clap(long)]
        multiplatform: bool,
    },
    /// Start Audio Features in CLI mode
    Audiofeatures {
        /// Path to music files (overrides config)
        #[clap(short, long)]
        path: PathBuf,

        /// Specify a path to config file
        #[clap(short, long)]
        config: String,

        /// Spotify Client ID
        #[clap(long)]
        client_id: String,

        /// Spotify Client Secret
        #[clap(long)]
        client_secret: String,

        /// Don't include subfolders
        #[clap(long)]
        no_subfolders: bool,
    },
    /// Query information about a URL without downloading
    QueryUrl {
        /// URL to query (YouTube, Spotify, or SoundCloud)
        #[clap(short, long)]
        url: String,
        
        /// Shazam confidence threshold (0.0-1.0)
        #[clap(long, default_value = "0.75")]
        confidence: f32,
    },
    /// Download songs from YouTube videos or playlists
    SongDownloader {
        /// YouTube URL (channel, playlist, or video)
        #[clap(short, long)]
        url: String,
        
        /// Output directory to save downloaded songs
        #[clap(short, long)]
        output: PathBuf,
        
        /// Shazam confidence threshold (0.0-1.0)
        #[clap(long, default_value = "0.75")]
        confidence: f32,
        
        /// Enable auto-tagging of downloaded songs
        #[clap(long)]
        enable_auto_tag: bool,
        
        /// Path to auto-tag configuration file
        #[clap(long)]
        auto_tag_config: Option<PathBuf>,
        
        /// Enable audio features analysis
        #[clap(long)]
        enable_audio_features: bool,
        
        /// Spotify Client ID (required for audio features)
        #[clap(long)]
        client_id: Option<String>,
        
        /// Spotify Client Secret (required for audio features)
        #[clap(long)]
        client_secret: Option<String>,
    },
    /// Authorize Spotify and cache the token
    AuthorizeSpotify {
        /// Spotify Client ID
        #[clap(long)]
        client_id: String,

        /// Spotify Client Secret
        #[clap(long)]
        client_secret: String,

        /// Run Spotify authentication callback server on `0.0.0.0`
        #[clap(long)]
        expose: bool,

        /// Don't start server, prompt for the redirected URL 
        #[clap(long)]
        prompt: bool
    },
    Renamer {
        /// Path to input files
        #[clap(long, short)]
        path: PathBuf,

        /// Output directory
        #[clap(long, short)]
        output: Option<PathBuf>,

        /// New filename template
        #[clap(long, short)]
        template: String,

        /// Copy files instead of moving
        #[clap(long)]
        copy: bool,

        /// Exclude subfolders 
        #[clap(long)]
        no_subfolders: bool,

        /// Don't actually affect files, only generate new names
        #[clap(long)]
        preview: bool,

        /// Overwrite files
        #[clap(long)]
        overwrite: bool,

        /// Multiple values separator
        #[clap(long, default_value = ", ")]
        separator: String,

        /// Keep original subfolders
        #[clap(long)]
        keep_subfolders: bool,
    },
    /// Start OneTagger server mode
    Server {
        /// Expose the internal servers (WARNING: Unsecure)
        #[clap(long, short)]
        expose: bool,
        /// Specify initial path to use in UI
        #[clap(long, short)]
        path: Option<String>,
        /// Open web browser
        #[clap(long, short)]
        browser: bool,
    }
}

/// For easily generating CLI -> config
macro_rules! config_option {
    ($target:expr, $t:tt) => {
        if *$t {
            $target.$t = *$t;
        }
    };
    ($target:expr, $($t:tt),+) => {
        $(config_option!($target, $t);)+
    }
}

impl Actions {
    //. Create tagger config
    pub fn get_at_config(&self) -> Result<TaggerConfig, Error> {
        match self {
            Actions::Autotagger { path, config, platforms, tags, id3v24, 
                overwrite, threads, strictness, album_art_file, merge_genres, camelot, 
                short_title, match_duration, max_duration_difference, match_by_id, enable_shazam, force_shazam, 
                skip_tagged, parse_filename, filename_template, no_subfolders, only_year, multiplatform } => {

                // Load config
                let mut config = if let Some(config_path) = config {
                    let config = serde_json::from_reader(&File::open(config_path)?)?;
                    config
                } else {
                    TaggerConfig::custom_default()
                };

                // Overrides
                config.path = Some(path.to_owned());
                if let Some(platforms) = platforms {
                    config.platforms = platforms.split(",").map(String::from).collect();
                }
                // Tags
                if let Some(tags) = tags {
                    let tags: Vec<SupportedTag> = tags
                        .split(",")
                        .filter_map(|t| {
                            match serde_json::from_str(&format!("\"{}\"", t.to_case(Case::Camel))) {
                                Ok(tag) => Some(tag),
                                Err(_) => {
                                    warn!("Invalid tag: {t}");
                                    None
                                },
                            }
                        })
                        .collect();
                    config.tags = tags;
                }
                // Boolean options
                config_option!(config, id3v24, overwrite, album_art_file, merge_genres, camelot, short_title, match_duration,
                    match_by_id, enable_shazam, force_shazam, skip_tagged, parse_filename, only_year, multiplatform);
                // Remaining options
                if let Some(threads) = threads {
                    config.threads = *threads;
                }
                if let Some(strictness) = strictness {
                    if *strictness > 100 {
                        warn!("Invalid stricness!");
                    } else {
                        config.strictness = *strictness as f64 / 100.0;
                    }
                }
                if let Some(mdd) = max_duration_difference {
                    config.max_duration_difference = *mdd;
                }
                if let Some(template) = filename_template {
                    config.filename_template = Some(template.to_string());
                }
                if *no_subfolders {
                    config.include_subfolders = false;
                }
                return Ok(config);
            },
            _ => unreachable!()
        }
    }
}
