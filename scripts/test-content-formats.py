"""Synthetic PDF pages and DOCX table extraction, no GUI or user files."""
import json
import subprocess
import tempfile
from pathlib import Path
from zipfile import ZipFile

helper = Path(__file__).resolve().parent.parent / "target/release/LuminatorContentExtractor"
with tempfile.TemporaryDirectory(prefix="luminator-formats-") as temporary:
    root = Path(temporary)
    swift = root / "fixture.swift"
    swift.write_text('''import AppKit
import CoreText
var box = CGRect(x: 0, y: 0, width: 612, height: 792)
let context = CGContext(URL(fileURLWithPath: CommandLine.arguments[1]) as CFURL, mediaBox: &box, nil)!
for text in ["第一页中文正文检索", "第二页海盐天空测试"] {
 context.beginPDFPage(nil)
 context.textPosition = CGPoint(x: 30, y: 720)
 let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: [.font: NSFont.systemFont(ofSize: 18)]))
 CTLineDraw(line, context)
 context.endPDFPage()
}
context.closePDF()
''')
    pdf = root / "document.pdf"
    subprocess.run(["swift", str(swift), str(pdf)], check=True)
    result = json.loads(subprocess.check_output([str(helper), str(pdf)]))
    assert result["state"] == "ready"
    assert result["parts"][1] == {"locator": "page:2", "text": "第二页海盐天空测试"}
    docx = root / "table.docx"
    with ZipFile(docx, "w") as archive:
        archive.writestr("[Content_Types].xml", '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>')
        archive.writestr("_rels/.rels", '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>')
        archive.writestr("word/document.xml", '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>文档段落提取测试</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>表格深海蓝鲸测试</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>')
    result = json.loads(subprocess.check_output([str(helper), str(docx)]))
    assert result["state"] == "ready"
    assert any("深海蓝鲸" in part["text"] for part in result["parts"])
    broken = root / "broken.pdf"
    broken.write_bytes(b"not a PDF")
    assert json.loads(subprocess.check_output([str(helper), str(broken)], stderr=subprocess.DEVNULL))["state"] == "unreadable"
    link = root / "link.pdf"
    link.symlink_to(pdf)
    assert json.loads(subprocess.check_output([str(helper), str(link)]))["state"] == "cloud_or_unavailable"
print("PASS: PDF page text, DOCX table, malformed PDF, symlink refusal")
