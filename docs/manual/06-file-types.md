# Supported File Types

[← Manual home](README.md)

---

## Text and source code

Plain text files and source code are indexed by content. Any file whose content is detected as text — regardless of extension — is handled by the text extractor.

**Explicitly recognized extensions** (indexed by content):

```
.txt  .md  .rst  .csv  .tsv  .log  .json  .yaml  .yml  .toml  .ini  .cfg
.xml  .html  .htm  .css  .js  .ts  .jsx  .tsx  .py  .rb  .go  .rs  .c
.cpp  .h  .hpp  .java  .kt  .swift  .sh  .bash  .zsh  .fish  .ps1
.sql  .r  .m  .scala  .clj  .ex  .exs  .erl  .hs  .lua  .vim  .tf
.proto  .graphql  .dockerfile  Dockerfile  Makefile  .env  ... and many more
```

**Markdown** — Frontmatter (YAML between `---` delimiters) is extracted and indexed alongside the document body. Title and description fields from frontmatter appear as metadata in the file viewer.

**Content detection** — Files without a recognized extension are sniffed for text content using byte-pattern analysis. UTF-8, Latin-1, and other encodings are detected automatically. Binary files that appear to be text are indexed; binary files that are clearly binary (high non-printable byte ratio) are skipped.

**Max file size** — Files larger than `scan.max_content_size_mb` (default: 10 MB) are indexed by filename only, without content.

---

## Documents

### PDF

PDF files are processed by a text extraction engine that recovers the text content from the PDF's internal representation.

- Text is extracted from each page and indexed in order
- Page numbers are preserved in the index
- The web UI can show both the extracted text view and render the original PDF inline
- Encrypted/password-protected PDFs are indexed by filename only; the viewer shows an "encrypted" indicator

**Common extraction issues:**

- **Scanned PDFs** — PDFs that are entirely scanned images contain no text layer. They are indexed by filename only.
- **Font encoding problems** — Some PDFs use custom font encodings that cannot be decoded. Affected pages may have missing or garbled text.
- **Unknown glyph warnings** — These are normal for PDFs with unusual fonts. They can be suppressed in `client.toml` via `log.ignore`.

### Office documents

| Format | Extracted content |
|---|---|
| `.docx` | Document body text |
| `.xlsx` | Cell values from all sheets |
| `.pptx` | Slide text content |

Older `.doc`, `.xls`, `.ppt` formats (Office 97–2003) are not currently supported.

### Apple iWork (.pages, .numbers, .key)

iWork files are ZIP-based documents. Text content is extracted natively.

- The embedded JPEG preview is extracted and shown in the image viewer
- Text is extracted from IWA (iWork Archive) protobuf files for modern iWork documents (.pages, .numbers, .key created in iWork 2013 or later)
- Older pre-2013 iWork documents (XML-based format) are also supported

### EPUB

EPUB files are extracted by reading the spine (the ordered list of content documents) and stripping HTML tags from each chapter. Metadata (title, author, language) is indexed as file-level metadata visible in the file viewer.

### HTML

HTML files have their tags stripped and their text content indexed. The `<title>` and `<meta name="description">` values are indexed as metadata.

---

## Archives

Archive files are opened and their members are extracted and indexed individually. Each member appears as a separate search result with a composite path using `::` as the separator:

```
taxes/2024.zip::W2.pdf
projects/backup.tar.gz::src/main.rs
data.zip::inner.zip::nested-file.txt
```

**Supported formats:**

| Format | Extension(s) |
|---|---|
| ZIP | `.zip` |
| Apple iWork | `.pages`, `.numbers`, `.key` |
| TAR | `.tar` |
| Gzipped TAR | `.tar.gz`, `.tgz` |
| Bzip2 TAR | `.tar.bz2`, `.tbz2` |
| XZ TAR | `.tar.xz`, `.txz` |
| Gzip | `.gz` (single file) |
| Bzip2 | `.bz2` (single file) |
| XZ | `.xz` (single file) |
| 7-Zip | `.7z` |

**Archive browsing in the UI** — Archive files expand in the file tree sidebar like directories. Members can be opened directly in the file viewer.

**Nested archives** — Archives within archives are extracted recursively up to `scan.archives.max_depth` (default: 10 levels). This prevents zip-bomb attacks while still supporting typical multi-level archive structures.

**7z solid archives** — 7z solid archives must decompress an entire solid block to access any member. The `scan.archives.max_7z_solid_block_mb` setting (default: 256 MB) caps how much memory this can use. Members in blocks that exceed the limit are indexed by filename only.

**Disabling archive indexing** — Set `scan.archives.enabled = false` to skip archive extraction entirely.

---

## Media

Media files are indexed by their embedded metadata rather than content (since audio/video content cannot be full-text searched).

### Images

Image metadata is extracted from EXIF, IPTC, and XMP tags embedded in the file. Indexed fields include:

- Camera make and model
- Date/time taken (used as the file date for search filtering)
- GPS coordinates (latitude, longitude, altitude)
- Image dimensions (width × height)
- Exposure, aperture, ISO, focal length
- Copyright and description

**Supported formats:** JPEG, TIFF, PNG, WebP, HEIF/HEIC, and other EXIF-capable formats.

### Audio

Audio metadata is extracted from tag fields:

| Format | Tags extracted |
|---|---|
| MP3 | ID3v1/v2: title, artist, album, year, genre, comment |
| FLAC | Vorbis comments: same fields |
| MP4/M4A | iTunes metadata: title, artist, album, year |
| OGG | Vorbis comments |

### Video

Basic video container metadata is extracted where available (title, duration, codec info). Video content is not transcribed.

---

## Windows executables

Windows PE (Portable Executable) files — `.exe`, `.dll`, `.sys` — are indexed by their embedded metadata:

- File description
- Product name and version
- Company name
- Original filename
- Legal copyright

This makes it possible to search for executables by their embedded product name or description rather than just their filename.

---

[← Web UI](05-web-ui.md) | [Next: Administration →](07-administration.md)
