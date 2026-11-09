import AppKit
import CoreServices
import SwiftUI


struct AssetsPanel: View {
    @ObservedObject var model: WorkspaceModel
    let snapshot: Snapshot
    @StoredState private var repairAsset: Snapshot.Document.Asset?

    private func changed(_ asset: Snapshot.Document.Asset) -> Bool {
        model.diagnostics?.files.contains { $0.path == asset.path && ($0.kind == "missing" || $0.kind == "modified") } == true
    }

    private func canRepair(_ asset: Snapshot.Document.Asset) -> Bool {
        guard let diagnostics = model.diagnostics, !diagnostics.manifestChanged,
              !diagnostics.files.isEmpty else { return false }
        return diagnostics.files.allSatisfy { $0.path == asset.path && ($0.kind == "missing" || $0.kind == "modified") }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Ресурсы проекта").font(.headline)
                Spacer()
                Button("Добавить ресурс…") { model.importAsset() }
                    .accessibilityIdentifier("import-asset")
                    .disabled(model.writesBlocked)
            }
            if snapshot.document.assets.isEmpty {
                ContentUnavailableView("Нет ресурсов", systemImage: "photo")
            } else {
                List(snapshot.document.assets) { asset in
                    HStack {
                        VStack(alignment: .leading) {
                            Text(asset.path).textSelection(.enabled)
                            Text(asset.id).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        }
                        Spacer()
                        Button(changed(asset) ? "Восстановить…" : "Перепривязать…") {
                            if changed(asset) { repairAsset = asset } else { model.relinkAsset(asset) }
                        }
                        .disabled((snapshot.readOnly || model.diagnostics?.readOnly == true || changed(asset)) && !canRepair(asset))
                    }
                }
            }
            if model.diagnostics?.files.contains(where: { $0.path.hasPrefix("assets/") }) == true {
                Text("При восстановлении сохранённая повреждённая версия помещается в conflicts; другие одновременные конфликты необходимо разрешить отдельно.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(20)
        .alert("Восстановить ресурс?", isPresented: Binding(get: { repairAsset != nil }, set: {
            if !$0 { repairAsset = nil }
        }), presenting: repairAsset) { asset in
            Button("Выбрать замену…") { model.relinkAsset(asset); repairAsset = nil }
            Button("Отмена", role: .cancel) { repairAsset = nil }
        } message: { asset in
            Text("Выберите замену для \(asset.path). Повреждённая версия будет сохранена в conflicts, если она доступна. Если файл уже отсутствует, его содержимое восстановить нельзя; отмена через эту операцию будет недоступна.")
        }
    }
}
