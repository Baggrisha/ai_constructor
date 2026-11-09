import AppKit
import CoreServices
import SwiftUI


struct WorkspaceView: View {
    @StateObject private var model = WorkspaceModel()
    @ObservedObject private var library = ProjectLibrary.shared
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @StoredState private var renameProject = false
    @StoredState private var projectName = ""
    @StoredState private var trashProject: RecentProject?
    @StoredState private var checkpointToRestore: Checkpoint?
    @StoredState private var confirmPortableSnapshot = false
    @StoredState private var searchOpen = false
    @StoredState private var sidebarTab = 0

    @ViewBuilder
    private func projectActions(_ project: RecentProject) -> some View {
        Button("Указать расположение…") { model.locate(project) }
        Button(project.archived == true ? "Вернуть из архива" : "В архив") {
            model.setArchived(project.archived != true, project: project)
        }
        if model.isSessionOpen(project) {
            Button("Закрыть сессию") { model.closeProject(project) }
        }
        Button("Убрать из списка") { model.removeRecent(project) }
        Divider()
        Button("Переместить в Корзину…", role: .destructive) { trashProject = project }
    }

    private func projectRow(_ project: RecentProject) -> some View {
        HStack {
            Button {
                model.open(project)
            } label: {
                VStack(alignment: .leading) {
                    Text(project.name)
                    Text(project.path).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("recent-project-\(project.path)")
            .contextMenu { projectActions(project) }
            Spacer(minLength: 0)
            Menu { projectActions(project) } label: { Image(systemName: "ellipsis.circle") }
                .menuStyle(.borderlessButton)
                .accessibilityLabel("Действия с проектом \(project.name)")
        }
    }

    @ViewBuilder
    private func panel(_ snapshot: Snapshot) -> some View {
        switch library.panels[snapshot.document.projectId] ?? .inspector {
        case .inspector:
            VStack(spacing: 0) {
                Picker("", selection: $sidebarTab) {
                    Text("Инспектор").tag(0)
                    Text("Слои").tag(1)
                    Text("Страницы").tag(2)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .padding(8)
                .accessibilityIdentifier("inspector-tabs")
                Group {
                    switch sidebarTab {
                    case 1: LayersPanel(model: model)
                    case 2: pagesList(snapshot)
                    default:
                        if snapshot.root != nil {
                            InspectorView(model: model, snapshot: snapshot)
                        } else {
                            ContentUnavailableView("Нет корневого фрейма", systemImage: "exclamationmark.triangle")
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        case .chats:
            ChatPanel(model: model, snapshot: snapshot).id(snapshot.document.projectId)
        case .assets:
            AssetsPanel(model: model, snapshot: snapshot)
        case .diagnostics:
            DiagnosticsPanel(model: model)
        }
    }

    /// The canvas takes the column above the page navigation; every chrome row
    /// is fixed height, so the header's intrinsic width never reaches layout.
    @ViewBuilder
    private func selectedProject(_ snapshot: Snapshot) -> some View {
        GeometryReader { geometry in
            VStack(spacing: 0) {
            HStack {
                VStack(alignment: .leading, spacing: 3) {
                    Text(snapshot.name).font(.headline)
                    statusText(snapshot).font(.caption).foregroundStyle(.secondary)
                }
                .lineLimit(1)
                Spacer(minLength: 12)
                projectButtons(snapshot).fixedSize()
            }
            .padding(.horizontal, 20)
            .frame(height: 50)
            Divider()
            diagnosticsChrome(snapshot)
            pageNavigation(snapshot)
                .frame(height: 44)
            HStack(spacing: 0) {
                panelColumn
                    .frame(width: max(0, geometry.size.width - library.panelWidths.inspector - 1), height: geometry.size.height, alignment: .topLeading)
                Rectangle().fill(.separator).frame(width: 1)
                panel(snapshot)
                    .frame(width: library.panelWidths.inspector, height: geometry.size.height, alignment: .topLeading)
            }
        }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .overlay(alignment: .topTrailing) {
            if searchOpen {
                SearchPanel(model: model, isPresented: $searchOpen)
                    .padding(16)
            }
        }
        .onAppear { WorkspaceCommands.shared.model = model }
        .onChange(of: WorkspaceCommands.shared.search) { _, open in
            if open { searchOpen = true; WorkspaceCommands.shared.search = false }
        }
        .onChange(of: searchOpen) { _, open in
            if !open { WorkspaceCommands.shared.search = false }
        }
    }

    private func pageName(_ page: Snapshot.Document.Page) -> String {
        model.rawNode(page.rootId)?["name"] as? String ?? page.id
    }

    private func pagesList(_ snapshot: Snapshot) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(snapshot.document.pages, id: \.id) { page in
                    Button {
                        model.selectPage(page.id)
                    } label: {
                        Label(pageName(page), systemImage: page.id == model.currentPageId ? "doc.text.fill" : "doc.text")
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .padding(.vertical, 4)
                    .accessibilityIdentifier("page-\(page.id)")
                }
                Text("Создание и удаление страниц — в следующих версиях")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
            }
            .padding(8)
        }
    }

    private func statusText(_ snapshot: Snapshot) -> Text {
        Text(model.textEditing ? "Редактирование текста" : model.hasUnsavedData ? "Не сохранено" : snapshot.readOnly ? "Только чтение" : model.diagnostics?.readOnly == true ? "Внешние изменения" : model.diagnosticsError != nil ? "Проверка не выполнена" : "Сохранено")
    }

    private func projectButtons(_ snapshot: Snapshot) -> some View {
        HStack(spacing: 8) {
            Button("Отменить", systemImage: "arrow.uturn.backward") { model.undo() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                .help("Отменить")
                .labelStyle(.iconOnly)
            Button("Повторить", systemImage: "arrow.uturn.forward") { model.redo() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
                .help("Повторить")
                .labelStyle(.iconOnly)
            Button("Сохранить", systemImage: "square.and.arrow.down") { model.save() }.disabled(model.writesBlocked || model.deferredFrameDraft != nil)
                .labelStyle(.iconOnly)
            Menu {
                actionsMenu(snapshot)
            } label: { Image(systemName: "ellipsis.circle") }
            .menuStyle(.borderlessButton)
            .fixedSize()
            .accessibilityLabel("Действия с проектом")
            .accessibilityIdentifier("project-actions")
        }
    }

    @ViewBuilder
    private func actionsMenu(_ snapshot: Snapshot) -> some View {
        Button("Закрыть проект") { model.closeProject() }
        Button("Переименовать проект…") { projectName = snapshot.name; renameProject = true }
            .disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
        Button("В архив") { model.archiveCurrent() }
        Button("Дублировать проект…") { model.duplicate() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
        Button("Создать переносимый снимок…") { confirmPortableSnapshot = true }
            .disabled(model.writesBlocked || model.hasUnsavedData || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
        Button("Создать контрольную точку") { model.createCheckpoint() }.disabled(model.writesBlocked || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
        Button("Обновить список контрольных точек") { model.refreshCheckpoints() }
            .disabled(model.writesBlocked)
        Menu("Восстановить контрольную точку") {
            ForEach(model.checkpoints) { checkpoint in
                Button("Ревизия \(checkpoint.revision) · \(checkpoint.id.prefix(8))") {
                    checkpointToRestore = checkpoint
                }
            }
        }
        .disabled(model.writesBlocked || model.checkpoints.isEmpty || model.hasBlockingFrameDraft || model.deferredFrameDraft != nil)
    }

    @ViewBuilder
    private func diagnosticsChrome(_ snapshot: Snapshot) -> some View {
        if let reason = snapshot.readOnlyReason {
            Text(reason)
                .font(.caption)
                .lineLimit(2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 20)
                .padding(.vertical, 6)
                .background(.orange.opacity(0.12))
        }
        if let diagnostics = model.diagnostics, diagnostics.manifestChanged || !diagnostics.files.isEmpty || !diagnostics.conflicts.isEmpty || diagnostics.blockedGenerated?.isEmpty == false {
            HStack(spacing: 8) {
                Label("Обнаружены внешние изменения или конфликты.", systemImage: "exclamationmark.triangle")
                    .lineLimit(1)
                Spacer(minLength: 8)
                Button("Открыть") { library.setPanel(.diagnostics, for: snapshot.document.projectId) }
                    .accessibilityIdentifier("open-diagnostics")
            }
            .font(.caption)
            .padding(.horizontal, 20)
            .padding(.vertical, 6)
            .background(.orange.opacity(0.12))
        }
        if model.diagnosticsError != nil || model.watchWarning != nil {
            HStack(spacing: 8) {
                Label(model.diagnosticsError ?? model.watchWarning ?? "", systemImage: "exclamationmark.triangle")
                    .lineLimit(1)
                Spacer(minLength: 8)
                Button("Открыть") { library.setPanel(.diagnostics, for: snapshot.document.projectId) }
            }
            .font(.caption)
            .padding(.horizontal, 20)
            .padding(.vertical, 6)
            .background(.orange.opacity(0.12))
        }
    }

    private func pageNavigation(_ snapshot: Snapshot) -> some View {
        Picker("Панель", selection: Binding(
            get: { library.panels[snapshot.document.projectId] ?? .inspector },
            set: { library.setPanel($0, for: snapshot.document.projectId) }
        )) {
            ForEach(WorkspacePanel.allCases, id: \.self) { value in
                Text(value.title).tag(value).accessibilityIdentifier("panel-\(value.rawValue)")
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .fixedSize()
        .accessibilityIdentifier("workspace-panel")
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 20)
    }

    /// The canvas sits beside the right-hand panel; it takes whatever width
    /// the two side panels leave.
    private var panelColumn: some View {
        CanvasView(model: model, content: model.canvas, patches: model.canvasPatches)
            .id("canvas-\(model.snapshot?.document.projectId ?? "none")")
    }


    @ViewBuilder
    private var detailColumn: some View {
        if let snapshot = model.snapshot {
            selectedProject(snapshot)
        } else {
            ContentUnavailableView {
                Label("Выберите проект", systemImage: "square.stack.3d.up")
            } description: {
                Text("Создайте проект или откройте существующую папку.")
            } actions: {
                Button("Создать проект") { model.create() }
                    .buttonStyle(.borderedProminent)
                Button("Открыть папку") { model.chooseProject() }
            }
        }
    }

    private var browserColumn: some View {
        VStack(alignment: .leading, spacing: 16) {
                Text("AI Constructor").font(.title2.bold()).padding(.top, 14)
                HStack {
                    Button("Новый", systemImage: "plus") { model.create() }
                        .accessibilityIdentifier("new-project")
                    Button("Открыть", systemImage: "folder") { model.chooseProject() }
                }
                TextField("Поиск проектов", text: $model.search)
                    .textFieldStyle(.roundedBorder)
                Text("НЕДАВНИЕ").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                List {
                    ForEach(library.recents.filter { $0.archived != true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }) { project in
                        projectRow(project)
                    }
                    let archived = library.recents.filter { $0.archived == true && (model.search.isEmpty || $0.name.localizedCaseInsensitiveContains(model.search)) }
                    if !archived.isEmpty {
                        Section("Архив") {
                            ForEach(archived) { project in projectRow(project) }
                        }
                    }
                }
                .listStyle(.sidebar)
            }
        .padding(.horizontal, 16)
        .frame(minWidth: 0, maxWidth: .infinity, minHeight: 0, maxHeight: .infinity, alignment: .topLeading)
        .background(SidebarMaterial())
    }

    var body: some View {
        GeometryReader { geometry in
            HStack(spacing: 0) {
                browserColumn
                    .frame(width: library.panelWidths.sidebar)
                    .clipped()
                Rectangle().fill(.separator).frame(width: 1)
                detailColumn
                    .frame(width: max(0, geometry.size.width - library.panelWidths.sidebar - 1), height: geometry.size.height, alignment: .topLeading)
            }
        }
        .background(WindowCloseGuard(model: model))
        .background(WindowSizingView())
        .onAppear { library.register(model) }
        .disabled(model.operation != nil && !model.conflictReviewPresented)
        .overlay(alignment: .bottom) {
            if let operation = model.operation {
                HStack {
                    ProgressView().controlSize(.small)
                    VStack(alignment: .leading) {
                        Text(operation)
                        if let message = model.cancellationMessage { Text(message).font(.caption) }
                    }
                    if model.canCancelOperation {
                        Button("Отменить операцию") { model.cancelOperation() }
                    }
                }
                .padding(12)
                .background {
                    if reduceTransparency {
                        RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .windowBackgroundColor))
                    } else {
                        RoundedRectangle(cornerRadius: 10).fill(.regularMaterial)
                    }
                }
                .padding()
            }
        }
        .task(id: model.snapshot?.document.projectId) {
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { break }
                await model.pollDiagnostics()
            }
        }
        .alert("Ошибка", isPresented: Binding(get: { !renameProject && !model.chatEditorPresented && !model.conflictReviewPresented && (model.error != nil || library.error != nil) }, set: {
            if !$0 { model.error = nil; library.error = nil }
        })) {
            Button("OK") { model.error = nil; library.error = nil }
        } message: { Text(model.error ?? library.error ?? "") }
        .alert("Переместить проект в Корзину?", isPresented: Binding(get: { trashProject != nil }, set: {
            if !$0 { trashProject = nil }
        }), presenting: trashProject) { project in
            Button("Переместить в Корзину", role: .destructive) { model.trash(project); trashProject = nil }
            Button("Отмена", role: .cancel) { trashProject = nil }
        } message: { project in
            Text("Папка \(project.path) будет перемещена в Корзину. Это отдельное действие и не совпадает с удалением записи из списка.")
        }
        .alert("Восстановить контрольную точку?", isPresented: Binding(get: { checkpointToRestore != nil }, set: {
            if !$0 { checkpointToRestore = nil }
        }), presenting: checkpointToRestore) { checkpoint in
            Button("Восстановить", role: .destructive) { model.restoreCheckpoint(checkpoint); checkpointToRestore = nil }
            Button("Отмена", role: .cancel) { checkpointToRestore = nil }
        } message: { checkpoint in
            Text("Будет восстановлена ревизия \(checkpoint.revision) вместе с файлами снимка. Текущие файлы проекта могут измениться.")
        }
        .alert("Восстановить файлы без права запуска?", isPresented: Binding(get: { model.checkpointAwaitingModeConfirmation != nil }, set: {
            if !$0 { model.checkpointAwaitingModeConfirmation = nil }
        }), presenting: model.checkpointAwaitingModeConfirmation) { checkpoint in
            Button("Восстановить без права запуска", role: .destructive) {
                model.checkpointAwaitingModeConfirmation = nil
                model.restoreCheckpoint(checkpoint, allowMissingModes: true)
            }
            Button("Отмена", role: .cancel) { model.checkpointAwaitingModeConfirmation = nil }
        } message: { _ in
            Text("В этой контрольной точке не сохранены права запуска отсутствующих файлов. Восстановить их без права запуска? Права существующих файлов сохранятся.")
        }
        .alert("Создать переносимый снимок?", isPresented: $confirmPortableSnapshot) {
            Button("Продолжить") { model.createPortableSnapshot() }
            Button("Отмена", role: .cancel) {}
        } message: {
            Text("Снимок включает файлы проекта, чаты, визуальные образцы и архивы конфликтов со старыми версиями файлов. Они могут содержать приватные данные. Проверьте содержимое перед публикацией или передачей. Несохранённые черновики чатов не включаются.")
        }
        .alert("Переносимый снимок создан", isPresented: Binding(get: { model.notice != nil }, set: {
            if !$0 { model.notice = nil }
        })) {
            Button("OK") { model.notice = nil }
        } message: { Text(model.notice ?? "") }
        .sheet(isPresented: $renameProject) {
            VStack(alignment: .leading, spacing: 16) {
                Text("Переименовать проект").font(.headline)
                TextField("Название", text: $projectName).disabled(model.operation != nil)
                if model.writesBlocked { Text("Сначала разрешите внешние изменения в диагностике.").foregroundStyle(.orange) }
                if let error = model.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
                if let operation = model.operation {
                    HStack {
                        ProgressView(operation)
                        if let message = model.cancellationMessage { Text(message).font(.caption) }
                        if model.canCancelOperation { Button("Отменить операцию") { model.cancelOperation() } }
                    }
                }
                HStack {
                    Spacer()
                    Button("Отмена") { model.error = nil; renameProject = false }.disabled(model.operation != nil)
                    Button("Сохранить") {
                        guard model.operation == nil else { return }
                        model.error = nil
                        model.rename(projectName) { renameProject = false }
                    }
                        .keyboardShortcut(.defaultAction)
                        .disabled(model.operation != nil || model.writesBlocked || projectName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }.padding(24).frame(width: 420)
                .interactiveDismissDisabled(model.operation != nil)
        }
    }
}
