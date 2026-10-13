// PDFKit and Cocoa extraction adapted from DigUp 6faa29e, MIT; see vendor/digup/LICENSE.
import AppKit
import Darwin
import Foundation
import PDFKit

struct Part: Codable { let locator: String; let text: String }
struct Reply: Codable { let state: String; let parts: [Part] }

@main struct ContentExtractor {
    static func main() {
        do {
            guard CommandLine.arguments.count == 2 else { throw CocoaError(.fileReadNoSuchFile) }
            guard setiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS,
                                IOPOL_MATERIALIZE_DATALESS_FILES_OFF) == 0 else {
                throw CocoaError(.fileReadNoPermission)
            }
            let url = URL(fileURLWithPath: CommandLine.arguments[1])
            var info = stat()
            guard lstat(url.path, &info) == 0, info.st_mode & S_IFMT == S_IFREG,
                  info.st_flags & UInt32(SF_DATALESS) == 0 else {
                emit(Reply(state: "cloud_or_unavailable", parts: [])); return
            }
            guard info.st_size <= 64 * 1024 * 1024 else {
                emit(Reply(state: "too_large", parts: [])); return
            }
            let fd = open(url.path, O_RDONLY | O_NOFOLLOW)
            guard fd >= 0 else { throw CocoaError(.fileReadNoPermission) }
            let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
            defer { try? handle.close() }
            guard fstat(fd, &info) == 0, info.st_mode & S_IFMT == S_IFREG,
                  info.st_flags & UInt32(SF_DATALESS) == 0,
                  info.st_size <= 64 * 1024 * 1024 else { throw CocoaError(.fileReadNoPermission) }
            let data = try handle.read(upToCount: 64 * 1024 * 1024 + 1) ?? Data()
            guard data.count <= 64 * 1024 * 1024 else { throw CocoaError(.fileReadTooLarge) }
            var parts: [Part] = []
            var count = 0
            var state = "ready"
            if url.pathExtension.lowercased() == "pdf" {
                guard let doc = PDFDocument(data: data) else { throw CocoaError(.fileReadCorruptFile) }
                if doc.isLocked { emit(Reply(state: "locked", parts: [])); return }
                for n in 0..<doc.pageCount {
                    let text = doc.page(at: n)?.string ?? ""
                    count += text.utf8.count
                    if count > 8 * 1024 * 1024 { state = "partial_limit"; break }
                    if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        parts.append(Part(locator: "page:\(n + 1)", text: text))
                    }
                }
            } else {
                let text = try NSAttributedString(data: data, options: [.documentType: NSAttributedString.DocumentType.officeOpenXML], documentAttributes: nil).string
                for (n, paragraph) in text.components(separatedBy: .newlines).enumerated() {
                    count += paragraph.utf8.count
                    if count > 8 * 1024 * 1024 { state = "partial_limit"; break }
                    if !paragraph.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        parts.append(Part(locator: "paragraph:\(n + 1)", text: paragraph))
                    }
                }
            }
            emit(Reply(state: parts.isEmpty && state == "ready" ? "no_text" : state, parts: parts))
        } catch { emit(Reply(state: "unreadable", parts: [])) }
    }
    static func emit(_ reply: Reply) {
        if let bytes = try? JSONEncoder().encode(reply) {
            FileHandle.standardOutput.write(bytes)
            FileHandle.standardOutput.write(Data([10]))
        }
    }
}
