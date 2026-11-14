//! Text out of the documents people attach to a prompt (PDF, Word, Excel, PowerPoint,
//! OpenDocument), so any model can read them, not only agents with a tool that opens them.

use std::io::{Cursor, Read};

/// A document part bigger than this is not read: a zip bomb, or not worth putting in a prompt.
const MAX_PART: u64 = 64 * 1024 * 1024;

/// The document's text, or `None` when the format is not one read here or it has no text
/// (a scanned PDF). `name` decides the format by its extension.
pub fn extract_text(name: &str, bytes: &[u8]) -> Option<String> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let text = match ext.as_str() {
        "pdf" => pdf(bytes),
        "docx" | "docm" | "dotx" => docx(bytes),
        "pptx" | "pptm" => pptx(bytes),
        "xlsx" | "xlsm" => xlsx(bytes),
        "odt" | "odp" => odf_text(bytes),
        "ods" => ods(bytes),
        _ => None,
    }?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn pdf(bytes: &[u8]) -> Option<String> {
    // The parser panics on some malformed files; a bad attachment must not take the turn down.
    std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
        .ok()?
        .map_err(|e| log::info!("documents: pdf: {e}"))
        .ok()
        .map(|text| tidy_lines(&text))
}

/// PDF text comes with runs of blank lines and trailing spaces.
fn tidy_lines(text: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        out.push_str(line);
    }
    out
}

fn open_zip(bytes: &[u8]) -> Option<zip::ZipArchive<Cursor<&[u8]>>> {
    zip::ZipArchive::new(Cursor::new(bytes)).ok()
}

fn part(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Option<String> {
    let file = zip.by_name(name).ok()?;
    if file.size() > MAX_PART {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_PART).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Parts named `<prefix><n>.xml`, in numeric order (slide10 after slide9).
fn numbered_parts(zip: &zip::ZipArchive<Cursor<&[u8]>>, prefix: &str) -> Vec<String> {
    let mut parts: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|name| {
            let name = name.ok()?;
            let n = name
                .strip_prefix(prefix)?
                .strip_suffix(".xml")?
                .parse()
                .ok()?;
            Some((n, name.to_string()))
        })
        .collect();
    parts.sort();
    parts.into_iter().map(|(_, name)| name).collect()
}

/// Paragraphs (`<w:p>`, `<a:p>`) of a WordprocessingML or DrawingML part, their runs'
/// `<t>` text joined, `<tab/>` and `<br/>` as whitespace.
fn paragraphs(xml: &str) -> Vec<String> {
    let Some(doc) = xml_doc(xml) else {
        return Vec::new();
    };
    doc.descendants()
        .filter(|n| n.tag_name().name() == "p")
        // A paragraph inside another (a text box in Word) is read with its outer one.
        .filter(|n| !n.ancestors().skip(1).any(|a| a.tag_name().name() == "p"))
        .map(|p| {
            let mut out = String::new();
            for n in p.descendants() {
                match n.tag_name().name() {
                    "t" => out.extend(n.children().filter_map(|c| c.text())),
                    "tab" => out.push('\t'),
                    "br" => out.push('\n'),
                    _ => {}
                }
            }
            out
        })
        .collect()
}

fn join_paragraphs(paragraphs: Vec<String>) -> String {
    paragraphs
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn docx(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let mut sections = vec![join_paragraphs(paragraphs(&part(
        &mut zip,
        "word/document.xml",
    )?))];
    for extra in ["word/footnotes.xml", "word/endnotes.xml"] {
        if let Some(xml) = part(&mut zip, extra) {
            sections.push(join_paragraphs(paragraphs(&xml)));
        }
    }
    Some(
        sections
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

fn pptx(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let slides = numbered_parts(&zip, "ppt/slides/slide");
    let mut out = Vec::new();
    for (i, name) in slides.iter().enumerate() {
        let Some(xml) = part(&mut zip, name) else {
            continue;
        };
        let text = join_paragraphs(paragraphs(&xml));
        let notes = name.replace("slides/slide", "notesSlides/notesSlide");
        let notes = part(&mut zip, &notes)
            .map(|xml| join_paragraphs(paragraphs(&xml)))
            .filter(|n| !n.is_empty());
        let mut slide = format!("## Slide {}\n{text}", i + 1);
        if let Some(notes) = notes {
            slide.push_str(&format!("\n\nNotes:\n{notes}"));
        }
        out.push(slide);
    }
    (!out.is_empty()).then(|| out.join("\n\n"))
}

fn xml_doc(xml: &str) -> Option<roxmltree::Document<'_>> {
    roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            ..Default::default()
        },
    )
    .ok()
}

/// The text of an element, its runs concatenated (`<si><r><t>a</t></r><r><t>b</t></r></si>`).
fn runs(node: roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.tag_name().name() == "t")
        .flat_map(|n| n.children().filter_map(|c| c.text()))
        .collect()
}

/// Sheets as tab-separated rows, each under its name.
fn xlsx(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let shared: Vec<String> = part(&mut zip, "xl/sharedStrings.xml")
        .and_then(|xml| {
            let doc = xml_doc(&xml)?;
            Some(
                doc.root_element()
                    .children()
                    .filter(|n| n.tag_name().name() == "si")
                    .map(runs)
                    .collect(),
            )
        })
        .unwrap_or_default();
    // Sheet names in workbook order, which is how `sheetN.xml` is usually numbered.
    let names: Vec<String> = part(&mut zip, "xl/workbook.xml")
        .and_then(|xml| {
            let doc = xml_doc(&xml)?;
            Some(
                doc.descendants()
                    .filter(|n| n.tag_name().name() == "sheet")
                    .filter_map(|n| n.attribute("name").map(str::to_string))
                    .collect(),
            )
        })
        .unwrap_or_default();
    let mut out = Vec::new();
    for (i, sheet) in numbered_parts(&zip, "xl/worksheets/sheet")
        .iter()
        .enumerate()
    {
        let Some(xml) = part(&mut zip, sheet) else {
            continue;
        };
        let Some(doc) = xml_doc(&xml) else {
            continue;
        };
        let rows: Vec<String> = doc
            .descendants()
            .filter(|n| n.tag_name().name() == "row")
            .map(|row| {
                let mut cells: Vec<String> = Vec::new();
                for c in row.children().filter(|n| n.tag_name().name() == "c") {
                    // Cells may skip empty columns: `r="D4"` says where this one goes.
                    if let Some(col) = c.attribute("r").and_then(column_index) {
                        while cells.len() < col {
                            cells.push(String::new());
                        }
                    }
                    let value = c
                        .children()
                        .find(|n| n.tag_name().name() == "v")
                        .and_then(|v| v.text())
                        .unwrap_or_default();
                    cells.push(match c.attribute("t") {
                        Some("s") => value
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i))
                            .cloned()
                            .unwrap_or_default(),
                        Some("inlineStr") => c
                            .children()
                            .find(|n| n.tag_name().name() == "is")
                            .map(runs)
                            .unwrap_or_default(),
                        Some("b") => if value == "1" { "TRUE" } else { "FALSE" }.to_string(),
                        _ => value.to_string(),
                    });
                }
                cells.join("\t").trim_end().to_string()
            })
            .filter(|r| !r.is_empty())
            .collect();
        if rows.is_empty() {
            continue;
        }
        let name = names
            .get(i)
            .cloned()
            .unwrap_or_else(|| format!("Sheet {}", i + 1));
        out.push(format!("## {name}\n{}", rows.join("\n")));
    }
    (!out.is_empty()).then(|| out.join("\n\n"))
}

/// `"D4"` → 3.
fn column_index(cell: &str) -> Option<usize> {
    let letters: String = cell.chars().take_while(char::is_ascii_alphabetic).collect();
    if letters.is_empty() {
        return None;
    }
    letters
        .chars()
        .try_fold(0usize, |n, ch| {
            Some(n * 26 + (ch.to_ascii_uppercase() as usize - 'A' as usize + 1))
        })
        .map(|n| n - 1)
}

/// OpenDocument text and presentations: headings and paragraphs in order.
fn odf_text(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let xml = part(&mut zip, "content.xml")?;
    let doc = xml_doc(&xml)?;
    let lines: Vec<String> = doc
        .descendants()
        .filter(|n| matches!(n.tag_name().name(), "p" | "h"))
        .filter(|n| {
            !n.ancestors()
                .skip(1)
                .any(|a| matches!(a.tag_name().name(), "p" | "h"))
        })
        .map(odf_paragraph)
        .filter(|p| !p.trim().is_empty())
        .collect();
    Some(lines.join("\n"))
}

fn odf_paragraph(p: roxmltree::Node<'_, '_>) -> String {
    let mut out = String::new();
    for n in p.descendants().skip(1) {
        if let Some(text) = n.is_text().then(|| n.text()).flatten() {
            out.push_str(text);
        }
        match n.tag_name().name() {
            "tab" => out.push('\t'),
            "line-break" => out.push('\n'),
            "s" => {
                let count = n
                    .attributes()
                    .find(|a| a.name() == "c")
                    .and_then(|a| a.value().parse().ok())
                    .unwrap_or(1);
                out.extend(std::iter::repeat_n(' ', count));
            }
            _ => {}
        }
    }
    out
}

/// OpenDocument spreadsheets: each table as tab-separated rows.
fn ods(bytes: &[u8]) -> Option<String> {
    let mut zip = open_zip(bytes)?;
    let xml = part(&mut zip, "content.xml")?;
    let doc = xml_doc(&xml)?;
    let mut out = Vec::new();
    for table in doc.descendants().filter(|n| n.tag_name().name() == "table") {
        let name = table
            .attributes()
            .find(|a| a.name() == "name")
            .map(|a| a.value().to_string())
            .unwrap_or_default();
        let rows: Vec<String> = table
            .descendants()
            .filter(|n| n.tag_name().name() == "table-row")
            .map(|row| {
                let mut cells = Vec::new();
                for cell in row
                    .children()
                    .filter(|n| n.tag_name().name() == "table-cell")
                {
                    let text = cell
                        .children()
                        .filter(|n| n.tag_name().name() == "p")
                        .map(odf_paragraph)
                        .collect::<Vec<_>>()
                        .join(" ");
                    // Empty cells repeat for the rest of a row: don't expand those.
                    let repeat = cell
                        .attributes()
                        .find(|a| a.name() == "number-columns-repeated")
                        .and_then(|a| a.value().parse::<usize>().ok())
                        .unwrap_or(1);
                    let repeat = if text.is_empty() {
                        repeat.min(1)
                    } else {
                        repeat.min(100)
                    };
                    cells.extend(std::iter::repeat_n(text, repeat));
                }
                cells.join("\t").trim_end().to_string()
            })
            .filter(|r| !r.is_empty())
            .collect();
        if !rows.is_empty() {
            out.push(format!("## {name}\n{}", rows.join("\n")));
        }
    }
    (!out.is_empty()).then(|| out.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    #[test]
    fn word_paragraphs_runs_and_tabs() {
        let doc = format!(
            r#"<w:document {W}><w:body>
                <w:p><w:r><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p>
                <w:p></w:p>
                <w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t></w:r></w:p>
            </w:body></w:document>"#
        );
        let bytes = zip_of(&[("word/document.xml", &doc)]);
        assert_eq!(
            extract_text("Report.DOCX", &bytes).unwrap(),
            "Hello world\na\tb"
        );
    }

    #[test]
    fn slides_in_numeric_order_with_notes() {
        let a = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main""#;
        let slide = |t: &str| {
            format!(r#"<p:sld xmlns:p="p" {a}><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#)
        };
        let bytes = zip_of(&[
            ("ppt/slides/slide10.xml", &slide("ten")),
            ("ppt/slides/slide2.xml", &slide("two")),
            ("ppt/notesSlides/notesSlide2.xml", &slide("say hi")),
        ]);
        assert_eq!(
            extract_text("deck.pptx", &bytes).unwrap(),
            "## Slide 1\ntwo\n\nNotes:\nsay hi\n\n## Slide 2\nten"
        );
    }

    #[test]
    fn sheets_with_shared_strings_and_gaps() {
        let ns = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;
        let shared = format!(
            r#"<sst {ns}><si><t>Name</t></si><si><r><t>Ana</t></r><r><t> M</t></r></si></sst>"#
        );
        let book = format!(r#"<workbook {ns}><sheets><sheet name="People"/></sheets></workbook>"#);
        let sheet = format!(
            r#"<worksheet {ns}><sheetData>
                <row><c r="A1" t="s"><v>0</v></c><c r="C1"><v>42</v></c></row>
                <row><c r="A2" t="s"><v>1</v></c><c r="B2" t="b"><v>1</v></c></row>
            </sheetData></worksheet>"#
        );
        let bytes = zip_of(&[
            ("xl/sharedStrings.xml", &shared),
            ("xl/workbook.xml", &book),
            ("xl/worksheets/sheet1.xml", &sheet),
        ]);
        assert_eq!(
            extract_text("data.xlsx", &bytes).unwrap(),
            "## People\nName\t\t42\nAna M\tTRUE"
        );
    }

    #[test]
    fn opendocument_text_and_sheets() {
        let ns = r#"xmlns:office="o" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="t""#;
        let odt = format!(
            r#"<office:document-content {ns}><office:body><office:text>
                <text:h>Title</text:h><text:p>one<text:s text:c="2"/>two</text:p>
            </office:text></office:body></office:document-content>"#
        );
        assert_eq!(
            extract_text("notes.odt", &zip_of(&[("content.xml", &odt)])).unwrap(),
            "Title\none  two"
        );
        let ods = format!(
            r#"<office:document-content {ns}><office:body><office:spreadsheet>
                <table:table table:name="S"><table:table-row>
                    <table:table-cell><text:p>x</text:p></table:table-cell>
                    <table:table-cell table:number-columns-repeated="1000"/>
                </table:table-row></table:table>
            </office:spreadsheet></office:body></office:document-content>"#
        );
        assert_eq!(
            extract_text("t.ods", &zip_of(&[("content.xml", &ods)])).unwrap(),
            "## S\nx"
        );
    }

    /// A one-page PDF saying `text`, with a correct cross-reference table.
    fn pdf_of(text: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", i + 1).bytes());
        }
        let xref = pdf.len();
        pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            pdf.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        pdf.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        pdf
    }

    #[test]
    fn pdf_text() {
        assert_eq!(
            extract_text("Invoice.PDF", &pdf_of("Total due: 42 EUR")).unwrap(),
            "Total due: 42 EUR"
        );
    }

    #[test]
    fn unknown_or_broken_files_have_no_text() {
        assert!(extract_text("photo.heic", b"...").is_none());
        assert!(extract_text("noext", b"...").is_none());
        assert!(extract_text("broken.docx", b"not a zip").is_none());
        assert!(extract_text("broken.pdf", b"%PDF-1.4 garbage").is_none());
    }

    #[test]
    fn column_letters() {
        assert_eq!(column_index("A1"), Some(0));
        assert_eq!(column_index("D4"), Some(3));
        assert_eq!(column_index("AA10"), Some(26));
        assert_eq!(column_index("12"), None);
    }
}
