import AppKit
import CoreServices
import SwiftUI


struct ConflictReview: Identifiable {
    let conflict: ProjectDiagnostics.Conflict
    let diskToken: String
    let localGeneration: UInt64
    var id: String { conflict.id }
}

struct DiagnosticsPanel: View {
    @ObservedObject var model: WorkspaceModel
    @StoredState private var review: ConflictReview?
    @StoredState private var confirmDiscardFrame = false
    @StoredState private var confirmDiscardDeferred = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("Диагностика проекта").font(.headline)
                Spacer()
                if model.diagnosticsInFlight {
                    ProgressView("Проверка…").controlSize(.small)
                    Button("Отменить проверку") { model.cancelBackgroundDiagnostics() }
                } else {
                    Button("Обновить") { Task { await model.pollDiagnostics() } }
                }
            }
            if model.frameDraft != nil {
                Label("Есть неразрешённый черновик фрейма. Отложите его или явно отбросьте до выбора версии.", systemImage: "exclamationmark.triangle")
                Button("Отложить черновик до разрешения") { model.deferFrameDraft() }
                    .accessibilityIdentifier("defer-frame-draft")
                    .disabled(model.operation != nil)
                Button("Отбросить черновик фрейма…") { confirmDiscardFrame = true }
                    .accessibilityIdentifier("discard-frame-draft")
                    .disabled(model.operation != nil)
            }
            if let deferred = model.deferredFrameDraft {
                Label("Черновик отложен в памяти. После выбора версии проверьте его перед применением.", systemImage: "doc.text")
                VStack(alignment: .leading) {
                    Text("Название: \(deferred.draft.name)")
                    Text("Ширина: \(deferred.draft.width)")
                    Text("Высота: \(deferred.draft.height)")
                }
                .textSelection(.enabled)
                HStack {
                    Button("Вернуть черновик") { model.restoreDeferredFrameDraft() }
                    Button("Отбросить отложенный черновик…") { confirmDiscardDeferred = true }
                }
                .disabled(model.operation != nil)
            }
            if let warning = model.watchWarning {
                Label(warning, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary)
            }
            if let error = model.diagnosticsError {
                Label(error, systemImage: "exclamationmark.triangle")
                    .textSelection(.enabled)
            } else if let diagnostics = model.diagnostics {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        if let reason = diagnostics.readOnlyReason {
                            Label(reason, systemImage: "lock").textSelection(.enabled)
                        }
                        if diagnostics.manifestChanged {
                            Label("Манифест изменён вне приложения.", systemImage: "exclamationmark.triangle")
                        }
                        if diagnostics.files.isEmpty && diagnostics.conflicts.isEmpty && !diagnostics.manifestChanged && diagnostics.blockedGenerated?.isEmpty != false {
                            Label("Внешние изменения не обнаружены.", systemImage: "checkmark.circle")
                        }
                        ForEach(diagnostics.files) { file in
                            VStack(alignment: .leading, spacing: 4) {
                                Text("\(file.kind): \(file.path)").fontWeight(.medium)
                                if let expected = file.expectedHash { Text("Ожидалось: \(expected)").font(.caption.monospaced()) }
                                if let actual = file.actualHash { Text("На диске: \(actual)").font(.caption.monospaced()) }
                            }
                            .textSelection(.enabled)
                        }
                        if let blocked = diagnostics.blockedGenerated, !blocked.isEmpty {
                            Label("Внешний сгенерированный код не синхронизирован с документом. Повторная генерация и восстановление, затрагивающее этот код, заблокированы; защита сохраняется в переносимом снимке.", systemImage: "exclamationmark.triangle")
                            ForEach(blocked, id: \.self) { path in Text(path).textSelection(.enabled) }
                        }
                        ForEach(diagnostics.conflicts) { conflict in
                            VStack(alignment: .leading) {
                                Text("Конфликт \(conflict.id)").fontWeight(.medium)
                                Text(conflict.reason)
                                if conflict.kind == "retainedWrite" {
                                    Text("Обнаружена поздняя запись в вытесненный файл. Её сохранённая версия может отличаться от текущего файла проекта.")
                                        .foregroundStyle(.secondary)
                                }
                                if ["externalChange", "retainedWrite"].contains(conflict.kind ?? ""),
                                   let paths = conflict.paths, !paths.isEmpty,
                                   paths.allSatisfy({ !$0.choices.isEmpty }),
                                   conflict.choices?.isEmpty == false,
                                   diagnostics.schemaVersion == 1,
                                   let diskToken = diagnostics.observedDiskToken,
                                   let generation = diagnostics.localGeneration {
                                    Button("Просмотреть и разрешить…") {
                                        model.conflictReviewPresented = true
                                        review = ConflictReview(conflict: conflict, diskToken: diskToken, localGeneration: generation)
                                    }
                                    .disabled(model.frameDraft != nil || model.operation != nil)
                                }
                                if conflict.kind == "recovery",
                                   let snapshots = conflict.snapshots,
                                   let choices = conflict.choices,
                                   (choices.contains("old") && snapshots.old.available) || (choices.contains("new") && snapshots.new.available),
                                   diagnostics.schemaVersion == 1,
                                   let diskToken = diagnostics.observedDiskToken,
                                   let generation = diagnostics.localGeneration {
                                    Button("Просмотреть варианты восстановления…") {
                                        model.conflictReviewPresented = true
                                        review = ConflictReview(conflict: conflict, diskToken: diskToken, localGeneration: generation)
                                    }
                                    .disabled(model.frameDraft != nil || model.operation != nil)
                                }
                            }
                            .textSelection(.enabled)
                        }
                        if diagnostics.manifestChanged || !diagnostics.files.isEmpty || !diagnostics.conflicts.isEmpty {
                            Text("Перед выбором сохраните копии внешних версий. Для конфликтов без доступных вариантов используйте проверенную резервную копию и затем заново откройте проект.")
                                .foregroundStyle(.secondary)
                        }
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ProgressView("Проверка проекта…")
            }
        }
        .padding(20)
        .sheet(item: $review, onDismiss: { model.conflictReviewPresented = false }) { item in
            if item.conflict.kind == "recovery" {
                RecoveryResolutionSheet(model: model, review: item)
            } else {
                ConflictResolutionSheet(model: model, review: item)
            }
        }
        .alert("Отбросить черновик фрейма?", isPresented: $confirmDiscardFrame) {
            Button("Отбросить черновик", role: .destructive) { model.discardFrameDraft() }
                .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Не применённый ввод в полях фрейма будет потерян. Уже применённые изменения документа не отменяются.")
        }
        .alert("Отбросить отложенный черновик?", isPresented: $confirmDiscardDeferred) {
            Button("Отбросить черновик", role: .destructive) { model.discardDeferredFrameDraft() }
                .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Отложенный ввод будет потерян. При необходимости сначала скопируйте значения из диагностики.")
        }
    }
}
