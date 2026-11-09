import SwiftUI

struct RecoveryResolutionSheet: View {
    @ObservedObject var model: WorkspaceModel
    let review: ConflictReview
    @Environment(\.dismiss) private var dismiss
    @StoredState private var take = ""
    @StoredState private var confirm = false

    private func snapshot(_ choice: String) -> ProjectDiagnostics.RecoverySnapshot? {
        guard let snapshots = review.conflict.snapshots else { return nil }
        return choice == "old" ? snapshots.old : snapshots.new
    }

    private func title(_ choice: String) -> String {
        choice == "old" ? "Предыдущий снимок" : "Новый снимок"
    }

    private var choices: [String] {
        ["old", "new"].filter {
            review.conflict.choices?.contains($0) == true && snapshot($0)?.available == true
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Восстановление после прерванной записи").font(.headline)
            Text("Выберите один проверенный снимок целиком. Смешивать файлы разных снимков нельзя.")
                .foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    ForEach(["old", "new"], id: \.self) { choice in
                        if let snapshot = snapshot(choice) {
                            VStack(alignment: .leading, spacing: 6) {
                                HStack {
                                    Text(title(choice)).fontWeight(.semibold)
                                    Text(snapshot.available ? "Доступен" : "Недоступен").foregroundStyle(.secondary)
                                    Spacer()
                                    if let path = snapshot.manifestPath {
                                        Button("Показать манифест") { model.revealConflictVersion(path) }
                                    }
                                }
                                if let revision = snapshot.revision { Text("Ревизия: \(revision)") }
                                if let generation = snapshot.storageGeneration { Text("Поколение хранения: \(generation)") }
                                if let hash = snapshot.manifestHash {
                                    Text("SHA-256 манифеста: \(hash)").font(.caption.monospaced()).textSelection(.enabled)
                                }
                                ForEach(snapshot.files ?? []) { file in
                                    HStack {
                                        VStack(alignment: .leading) {
                                            Text(file.path).textSelection(.enabled)
                                            if let hash = file.hash { Text(hash).font(.caption.monospaced()).textSelection(.enabled) }
                                        }
                                        Spacer()
                                        if let path = file.archiveRelativePath {
                                            Button("Показать копию") { model.revealConflictVersion(path) }
                                        }
                                    }
                                }
                            }
                            Divider()
                        }
                    }
                }
            }
            .disabled(model.operation != nil)
            Picker("Восстановить", selection: $take) {
                Text("Выберите снимок").tag("")
                ForEach(choices, id: \.self) { choice in Text(title(choice)).tag(choice) }
            }
            .disabled(model.operation != nil)
            if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
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
                Button("Восстановить…") { confirm = true }
                    .buttonStyle(.borderedProminent)
                    .disabled(!choices.contains(take) || model.operation != nil)
            }
        }
        .padding(24)
        .frame(minWidth: 620, minHeight: 440)
        .interactiveDismissDisabled(model.operation != nil)
        .alert("Восстановить выбранный снимок?", isPresented: $confirm) {
            Button("Восстановить", role: .destructive) {
                model.resolveRecovery(review, take: take) { dismiss() }
            }
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Будет восстановлен \(title(take).lowercased()) целиком. Перед записью приложение проверит актуальность файлов и сохранит неизвестные версии.")
        }
    }
}
