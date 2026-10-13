// Text-only adaptation of DigUp LlamaEmbedder.swift at 6faa29e; MIT, vendor/digup/LICENSE.
import CLlama
import Foundation

struct EmbeddingFailure: Error { let message: String }
struct Input: Decodable { let texts: [String] }

@main struct TextEmbedder {
    static func emit(_ value: [String: Any]) throws {
        let data = try JSONSerialization.data(withJSONObject: value)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([10]))
    }
    static func main() throws {
        guard CommandLine.arguments.count == 2 else { throw EmbeddingFailure(message: "model path required") }
        llama_log_set({ level, message, _ in
            if level.rawValue >= 3, let message {
                FileHandle.standardError.write(Data(String(cString: message).utf8))
            }
        }, nil)
        let started = Date()
        llama_backend_init()
        defer { llama_backend_free() }
        var mp = llama_model_default_params()
        mp.n_gpu_layers = 999
        guard let model = llama_model_load_from_file(CommandLine.arguments[1], mp) else {
            throw EmbeddingFailure(message: "model load failed")
        }
        defer { llama_model_free(model) }
        let capacity = 2048
        var cp = llama_context_default_params()
        cp.n_ctx = UInt32(capacity)
        cp.n_batch = UInt32(capacity)
        cp.n_ubatch = UInt32(capacity)
        cp.n_seq_max = 1
        cp.n_threads = 4
        cp.n_threads_batch = 4
        cp.kv_unified = true
        cp.embeddings = true
        cp.pooling_type = LLAMA_POOLING_TYPE_MEAN
        guard let context = llama_init_from_model(model, cp) else { throw EmbeddingFailure(message: "context failed") }
        defer { llama_free(context) }
        let vocab = llama_model_get_vocab(model)
        let batch = llama_batch_ext_init(context)
        defer { llama_batch_ext_free(batch) }
        let dimensions = Int(llama_model_n_embd_out(model))
        try emit(["ready": true, "dimensions": dimensions, "load_ms": Date().timeIntervalSince(started) * 1000,
                  "max_tokens": capacity, "runtime": "llama.cpp-b11461-digup-no-logits", "gpu": true])
        while let line = readLine() {
            do {
                let request = try JSONDecoder().decode(Input.self, from: Data(line.utf8))
                guard request.texts.count <= 64 else { throw EmbeddingFailure(message: "at most 64 texts per request") }
                let begin = Date()
                var vectors: [[Float]] = []
                var lengths: [Int] = []
                for text in request.texts {
                    let bytes = Array(text.utf8CString)
                    var tokens = [llama_token](repeating: 0, count: bytes.count + 8)
                    let count = llama_tokenize(vocab, bytes, Int32(bytes.count - 1), &tokens, Int32(tokens.count), true, true)
                    guard count > 0, count <= capacity else {
                        throw EmbeddingFailure(message: "input exceeds 2048 tokens; split the document instead of truncating")
                    }
                    lengths.append(Int(count))
                    llama_batch_ext_clear(batch)
                    for n in 0..<Int(count) {
                        let index = llama_batch_ext_add_token(batch, 0, tokens[n])
                        var position: [llama_pos] = [Int32(n), 0, 0, 0]
                        guard index >= 0, llama_batch_ext_set_pos(batch, index, &position),
                              llama_batch_ext_set_output_embd(batch, index, true) else {
                            throw EmbeddingFailure(message: "cannot fill embedding batch")
                        }
                    }
                    guard llama_process(context, LLAMA_PROCESS_TYPE_ENCODE, batch) == 0,
                          let pooled = llama_get_embeddings_seq(context, 0) else {
                        throw EmbeddingFailure(message: "encoding failed")
                    }
                    let vector = Array(UnsafeBufferPointer(start: pooled, count: dimensions))
                    let norm = vector.reduce(0.0) { $0 + Double($1) * Double($1) }.squareRoot()
                    guard norm.isFinite && norm > 0 else { throw EmbeddingFailure(message: "invalid embedding") }
                    vectors.append(vector.map { $0 / Float(norm) })
                }
                try emit(["vectors": vectors, "tokens": lengths, "elapsed_ms": Date().timeIntervalSince(begin) * 1000])
            } catch {
                try emit(["error": (error as? EmbeddingFailure)?.message ?? "invalid request"])
            }
        }
    }
}
