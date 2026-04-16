use egui::Color32;

/// File category for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Directory,
    Image,
    Video,
    Audio,
    Archive,
    Document,
    Code,
    Executable,
    Font,
    Data,
    Other,
}

impl FileCategory {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            // Images
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tiff" | "tif"
            | "webp" | "svg" | "ico" | "raw" | "cr2" | "nef" | "heic"
            | "avif" | "jxl" => FileCategory::Image,

            // Videos
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm"
            | "m4v" | "mpg" | "mpeg" | "vob" | "3gp" => FileCategory::Video,

            // Audio
            "mp3" | "flac" | "wav" | "ogg" | "aac" | "m4a" | "wma"
            | "opus" | "aiff" | "ape" | "mka" | "mid" | "midi" => FileCategory::Audio,

            // Archives
            "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" | "rar"
            | "zst" | "lz4" | "lzma" | "cab" | "iso" | "dmg"
            | "deb" | "rpm" | "pkg" | "apk" => FileCategory::Archive,

            // Documents
            "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods"
            | "ppt" | "pptx" | "odp" | "txt" | "rtf" | "md" | "rst"
            | "tex" | "epub" | "mobi" | "djvu" | "fb2" => FileCategory::Document,

            // Code / Scripts
            "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "cpp"
            | "cc" | "cxx" | "h" | "hpp" | "java" | "go" | "rb"
            | "php" | "sh" | "bash" | "zsh" | "fish" | "ps1"
            | "swift" | "kt" | "cs" | "lua" | "pl" | "hs"
            | "dart" | "elm" | "ex" | "exs" | "clj" | "scala"
            | "r" | "m" | "f90" | "f95" | "asm" | "s"
            | "html" | "htm" | "css" | "scss" | "sass" | "less"
            | "xml" | "json" | "yaml" | "yml" | "toml" | "ini"
            | "cfg" | "conf" | "sql" | "graphql" | "proto" => FileCategory::Code,

            // Executables / Binaries
            "so" | "dll" | "dylib" | "exe" | "bin" | "o" | "a"
            | "lib" | "out" | "elf" => FileCategory::Executable,

            // Fonts
            "ttf" | "otf" | "woff" | "woff2" | "eot" | "fon" => FileCategory::Font,

            // Data
            "db" | "sqlite" | "sqlite3" | "csv" | "tsv" | "parquet"
            | "arrow" | "avro" | "hdf5" | "h5" | "nc" | "mat" => FileCategory::Data,

            _ => FileCategory::Other,
        }
    }

    /// Base color in dark mode.
    pub fn dark_color(self) -> Color32 {
        match self {
            FileCategory::Directory  => Color32::from_rgb(52,  88,  130),  // steel blue
            FileCategory::Image      => Color32::from_rgb(45,  130, 70),   // forest green
            FileCategory::Video      => Color32::from_rgb(120, 50,  160),  // purple
            FileCategory::Audio      => Color32::from_rgb(40,  100, 180),  // cobalt blue
            FileCategory::Archive    => Color32::from_rgb(180, 90,  30),   // burnt orange
            FileCategory::Document   => Color32::from_rgb(30,  140, 140),  // teal
            FileCategory::Code       => Color32::from_rgb(160, 140, 20),   // golden
            FileCategory::Executable => Color32::from_rgb(160, 40,  40),   // red
            FileCategory::Font       => Color32::from_rgb(80,  120, 80),   // muted green
            FileCategory::Data       => Color32::from_rgb(70,  110, 150),  // slate
            FileCategory::Other      => Color32::from_rgb(70,  70,  80),   // dark gray
        }
    }

    /// Highlighted (hovered) color – brighter version.
    pub fn dark_color_hover(self) -> Color32 {
        let c = self.dark_color();
        lighten(c, 60)
    }

    /// Selected color.
    pub fn dark_color_selected(self) -> Color32 {
        let c = self.dark_color();
        lighten(c, 40)
    }

    /// Light mode base color.
    pub fn light_color(self) -> Color32 {
        match self {
            FileCategory::Directory  => Color32::from_rgb(120, 170, 220),
            FileCategory::Image      => Color32::from_rgb(100, 200, 130),
            FileCategory::Video      => Color32::from_rgb(190, 120, 230),
            FileCategory::Audio      => Color32::from_rgb(100, 160, 240),
            FileCategory::Archive    => Color32::from_rgb(240, 160, 80),
            FileCategory::Document   => Color32::from_rgb(80,  210, 210),
            FileCategory::Code       => Color32::from_rgb(230, 210, 80),
            FileCategory::Executable => Color32::from_rgb(230, 90,  90),
            FileCategory::Font       => Color32::from_rgb(140, 200, 140),
            FileCategory::Data       => Color32::from_rgb(130, 180, 220),
            FileCategory::Other      => Color32::from_rgb(160, 160, 170),
        }
    }

    pub fn light_color_hover(self) -> Color32 {
        let c = self.light_color();
        darken(c, 40)
    }

    pub fn label(self) -> &'static str {
        match self {
            FileCategory::Directory  => "Directory",
            FileCategory::Image      => "Image",
            FileCategory::Video      => "Video",
            FileCategory::Audio      => "Audio",
            FileCategory::Archive    => "Archive",
            FileCategory::Document   => "Document",
            FileCategory::Code       => "Source Code",
            FileCategory::Executable => "Executable",
            FileCategory::Font       => "Font",
            FileCategory::Data       => "Data",
            FileCategory::Other      => "Other",
        }
    }
}

pub fn get_category(entry: &crate::scanner::FileEntry) -> FileCategory {
    if entry.is_unscanned {
        return FileCategory::Other; // rendered specially in app.rs
    }
    if entry.is_dir {
        return FileCategory::Directory;
    }
    match entry.extension() {
        Some(ext) => FileCategory::from_extension(ext),
        None => FileCategory::Other,
    }
}

fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(amount),
        c.g().saturating_add(amount),
        c.b().saturating_add(amount),
    )
}

fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_sub(amount),
        c.g().saturating_sub(amount),
        c.b().saturating_sub(amount),
    )
}
