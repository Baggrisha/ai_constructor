import SwiftUI

struct ConflictResolutionSheet: View {
    @ObservedObject var model: WorkspaceModel
    let review: ConflictReview
    @Environment(\.dismiss) private var dismiss
    @StoredState private var selections: [String: String] = [:]
    @StoredState private var confirm = false

    private var paths: [ProjectDiagnostics.ConflictPath] { review.conflict.paths ?? [] }

    private func title(_ choice: String) -> String {
        switch choice {
        case "base": return "Исходная версия"
        case "local": return "Версия редактора"
        case "disk": return review.conflict.kind == "retainedWrite" ? "Поздняя запись во внешний дескриптор" : "Версия на диске"
        default: return choice
        }
    }

    private func version(_ choice: String, in path: ProjectDiagnostics.ConflictPath) -> ProjectDiagnostics.Version {
        switch choice {
        case "base": return path.base
        case "local": return path.local
        default: return path.disk
        }
    }

    private var complete: Bool {
        !paths.isEmpty && paths.allSatisfy { path in
            guard let selected = selections[path.path] else { return false }
            return ["base", "local", "disk"].contains(selected) && path.choices.contains(selected) && review.conflict.choices?.contains(selected) == true
        }
    }

    private var removedPaths: [String] {
        paths.compactMap { path in
            guard let selected = selections[path.path], version(selected, in: path).state == "absent" else { return nil }
            return path.path
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Разрешение конфликта").font(.headline)
            Text("Выберите версию для каждого файла. Никакая версия не выбирается автоматически.")
                .foregroundStyle(.secondary)
            if review.conflict.kind == "retainedWrite" {
                Text("Поздняя запись получена из вытеснённого открытого файла, а не из текущего файла проекта.")
                    .foregroundStyle(.secondary)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    ForEach(paths) { path in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(path.path).fontWeight(.semibold).textSelection(.enabled)
                            ForEach(["base", "local", "disk"], id: \.self) { choice in
                                let item = version(choice, in: path)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack {
                                        Text(title(choice)).fontWeight(.medium)
                                        Text(item.state == "absent" ? "Файл отсутствует" : item.state == "unavailable" ? "Версия недоступна" : "Сохранённая копия")
                                            .foregroundStyle(.secondary)
                                        Spacer()
                                        if let archive = item.archiveRelativePath {
                                            Button("Показать копию в Finder") { model.revealConflictVersion(archive) }
                                        }
                                    }
                                    if let hash = item.hash ?? item.expectedHash {
                                        Text("SHA-256: \(hash)").font(.caption.monospaced()).textSelection(.enabled)
                                    }
                                    if let size = item.size { Text("Размер: \(size) байт").font(.caption) }
                                }
                            }
                            Picker("Использовать", selection: Binding(
                                get: { selections[path.path] ?? "" },
                                set: { selections[path.path] = $0 }
                            )) {
                                Text("Выберите версию").tag("")
                                ForEach(path.choices.filter { ["base", "local", "disk"].contains($0) && review.conflict.choices?.contains($0) == true }, id: \.self) { choice in
                                    Text(title(choice)).tag(choice)
                                }
                            }
                        }
                        Divider()
                    }
                }
            }
            .disabled(model.operation != nil)
            if let error = model.error {
                Text(error).foregroundStyle(.red).textSelection(.enabled)
            }
            if let operation = model.operation {
                HStack {
                    ProgressView(operation)
                    if let message = model.cancellationMessage { Text(message).font(.caption) }
                    if model.canCancelOperation {
                        Button("Отменить операцию") { model.cancelOperation() }
                    }
                }
            }
            HStack {
                Spacer()
                Button("Закрыть") { dismiss() }.disabled(model.operation != nil)
                Button("Применить выбор…") { confirm = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(!complete || model.operation != nil)
            }
        }
        .padding(24)
        .frame(minWidth: 620, minHeight: 440)
        .interactiveDismissDisabled(model.operation != nil)
        .alert("Применить выбранные версии?", isPresented: $confirm) {
            Button("Применить") {
                model.resolveConflict(review, selections: selections) { dismiss() }
            }
            Button("Отмена", role: .cancel) {}
        } message: {
            if removedPaths.isEmpty {
                Text("Приложение проверит, что ни одна версия не изменилась после открытия этого окна.")
            } else {
                Text("Для выбранной отсутствующей версии будут удалены пути: \(removedPaths.joined(separator: ", ")). Приложение проверит актуальность версий перед записью.")
            }
        }
    }
}
