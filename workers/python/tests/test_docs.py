"""Documents imported as notes (columbus_docs): Word, Excel, PDF, CSV and
text become Markdown; crafted files are refused rather than read without
limit."""

import os
import sys
import tempfile
import unittest
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)

import columbus_docs as d  # noqa: E402
import columbus_worker as w  # noqa: E402

WNS = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"'


def para(text, style=None, listed=False):
    ppr = ""
    if style or listed:
        ppr = "<w:pPr>" + (f'<w:pStyle w:val="{style}"/>' if style else "") + ("<w:numPr><w:ilvl w:val=\"0\"/></w:numPr>" if listed else "") + "</w:pPr>"
    return f"<w:p>{ppr}<w:r><w:t>{text}</w:t></w:r></w:p>"


def docx(path, body, core_title=None, extra=None):
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("word/document.xml", f'<?xml version="1.0"?><w:document {WNS}><w:body>{body}</w:body></w:document>')
        if core_title:
            z.writestr("docProps/core.xml", f'<cp:coreProperties xmlns:cp="x" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>{core_title}</dc:title></cp:coreProperties>')
        for k, v in (extra or {}).items():
            z.writestr(k, v)


class DocsTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.dir.cleanup()

    def p(self, name):
        return os.path.join(self.dir.name, name)

    def test_word_headings_lists_tables(self):
        body = (para("Monthly LRS check", "Title") + para("Steps", "Heading1") + para("Take active segments", listed=True)
                + para("Sort by ‮measure", listed=True) + para("Plain text here.")
                + "<w:tbl><w:tr><w:tc>" + para("Code") + "</w:tc><w:tc>" + para("Meaning") + "</w:tc></w:tr>"
                + "<w:tr><w:tc>" + para("A") + "</w:tc><w:tc>" + para("Active | in use") + "</w:tc></w:tr></w:tbl>")
        docx(self.p("qc.docx"), body, extra={"word/vbaProject.bin": "x"})
        r = d.convert_document({"path": self.p("qc.docx")})
        md = r["markdown"]
        self.assertEqual(r["title"], "Monthly LRS check")
        self.assertIn("# Monthly LRS check", md)
        self.assertIn("## Steps", md)
        self.assertIn("- Take active segments", md)
        self.assertIn("- Sort by measure", md, "invisible formatting characters are dropped")
        self.assertIn("| Code | Meaning |", md)
        self.assertIn("Active \\| in use", md)
        self.assertTrue(any("macros" in x for x in r["warnings"]))
        self.assertEqual(len(r["sha256"]), 64)

    def test_word_entities_are_refused(self):
        bomb = '<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa"><!ENTITY b "&a;&a;&a;">]>' + f"<w:document {WNS}><w:body>{para('&b;')}</w:body></w:document>"
        with zipfile.ZipFile(self.p("bomb.docx"), "w") as z:
            z.writestr("word/document.xml", bomb)
        with self.assertRaises(w.WorkerError):
            d.convert_document({"path": self.p("bomb.docx")})
        # Not a ZIP at all.
        with open(self.p("fake.docx"), "w") as f:
            f.write("hello")
        with self.assertRaises(w.WorkerError):
            d.convert_document({"path": self.p("fake.docx")})

    def test_a_part_larger_than_allowed_is_refused(self):
        with zipfile.ZipFile(self.p("big.docx"), "w", zipfile.ZIP_DEFLATED) as z:
            z.writestr("word/document.xml", "<a>" + " " * (d.MAX_PART + 10) + "</a>")
        with self.assertRaises(w.WorkerError):
            d.convert_document({"path": self.p("big.docx")})

    def test_excel_sheets_as_tables(self):
        S = 'xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"'
        R = 'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
        with zipfile.ZipFile(self.p("codes.xlsx"), "w") as z:
            z.writestr("xl/workbook.xml", f'<workbook {S} {R}><sheets><sheet name="Codes" sheetId="1" r:id="rId1"/><sheet name="Evil" sheetId="2" r:id="rId2"/></sheets></workbook>')
            z.writestr("xl/_rels/workbook.xml.rels", '<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Target="../../etc/passwd"/></Relationships>')
            z.writestr("xl/sharedStrings.xml", f'<sst {S}><si><t>Code</t></si><si><t>Meaning</t></si><si><t>Retired</t></si></sst>')
            z.writestr("xl/worksheets/sheet1.xml", f'<worksheet {S}><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="A2"><v>7</v></c><c r="C2" t="s"><v>2</v></c></row></sheetData></worksheet>')
        md = d.convert_document({"path": self.p("codes.xlsx")})["markdown"]
        self.assertIn("## Codes", md)
        self.assertIn("| Code | Meaning |", md)
        self.assertIn("| 7 |  | Retired |", md)
        self.assertNotIn("Evil", md)

    def test_csv_markdown_text(self):
        with open(self.p("t.csv"), "w") as f:
            f.write("a;b\n1;2\n")
        self.assertIn("| a | b |", d.convert_document({"path": self.p("t.csv")})["markdown"])
        with open(self.p("n.md"), "w") as f:
            f.write("# Notes\n\nHello\r\n")
        r = d.convert_document({"path": self.p("n.md")})
        self.assertEqual(r["title"], "Notes")
        with self.assertRaises(w.WorkerError):
            d.convert_document({"path": self.p("x.exe")})

    def test_pdf(self):
        try:
            from pypdf import PdfWriter
        except ImportError:
            self.skipTest("pypdf not installed")
        wr = PdfWriter()
        wr.add_blank_page(200, 200)
        wr.add_metadata({"/Title": "Blank"})
        with open(self.p("b.pdf"), "wb") as f:
            wr.write(f)
        r = d.convert_document({"path": self.p("b.pdf")})
        self.assertEqual(r["title"], "Blank")
        self.assertTrue(any("scan" in x for x in r["warnings"]))
        with open(self.p("bad.pdf"), "wb") as f:
            f.write(b"%PDF-1.4 nonsense")
        with self.assertRaises(w.WorkerError):
            d.convert_document({"path": self.p("bad.pdf")})


if __name__ == "__main__":
    unittest.main()
