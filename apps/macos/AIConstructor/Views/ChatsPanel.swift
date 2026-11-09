import AppKit
import CoreServices
import SwiftUI


struct ChatPanel: View {
    @ObservedObject var model: WorkspaceModel
    let snapshot: Snapshot
    @StoredState private var showChatSheet = false
    @StoredState private var editingChat: Chat?
    @StoredState private var forkOrigin: Chat?
    @StoredState private var forkMessage: ChatMessage?
    @StoredState private var deleteCandidate: Chat?
    @StoredState private var title = ""
    @StoredState private var mode = "discuss"
    @StoredState private var search = ""

    private var selected: Chat? {
        model.chats.first { $0.id == model.selectedChatId }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Picker("Чат", selection: Binding(
                    get: { model.selectedChatId ?? "" },
                    set: { model.chatSearchResults = nil; model.refreshChats(select: $0) }
                )) {
                    Text("Выберите чат").tag("")
                    ForEach(model.chats) { chat in
                        Text(chat.archived ? "\(chat.title) · архив" : chat.title).tag(chat.id)
                    }
                }
                .accessibilityIdentifier("chat-picker")
                Button("Новый чат", systemImage: "plus") {
                    editingChat = nil
                    forkOrigin = nil
                    forkMessage = nil
                    title = ""
                    mode = "discuss"
                    showChatSheet = true
                }
                .disabled(model.writesBlocked)
                if let selected {
                    Menu("Действия с чатом") {
                        Button("Переименовать…") {
                            editingChat = selected
                            forkOrigin = nil
                            forkMessage = nil
                            title = selected.title
                            mode = selected.mode
                            showChatSheet = true
                        }
                        Button(selected.archived ? "Вернуть из архива" : "В архив") {
                            model.archiveChat(selected, archived: !selected.archived)
                        }
                        Menu("Режим") {
                            Button("Обсудить") { model.setChatMode(selected, mode: "discuss") }
                            Button("План") { model.setChatMode(selected, mode: "plan") }
                            Button("Изменить") { model.setChatMode(selected, mode: "edit") }
                        }
                        Divider()
                        Button("Удалить чат…", role: .destructive) { deleteCandidate = selected }
                    }
                    .disabled(model.writesBlocked)
                }
            }
            Text("Локальная история чата. Ответы ИИ в этом этапе не подключены.")
                .font(.caption).foregroundStyle(.secondary)
            HStack {
                TextField("Поиск по всем чатам проекта", text: $search)
                    .textFieldStyle(.roundedBorder)
                Button("Искать") { model.searchChats(search.trimmingCharacters(in: .whitespacesAndNewlines)) }
                    .disabled(search.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                if model.chatSearchResults != nil {
                    Button("Закрыть поиск") { model.chatSearchResults = nil }
                }
            }
            if let results = model.chatSearchResults {
                if results.isEmpty {
                    ContentUnavailableView("Сообщения не найдены", systemImage: "magnifyingglass")
                } else {
                    List(results) { hit in
                        Button {
                            model.chatSearchResults = nil
                            model.refreshChats(select: hit.chat.id)
                        } label: {
                            VStack(alignment: .leading) {
                                Text(hit.chat.archived ? "\(hit.chat.title) · архив" : hit.chat.title).fontWeight(.medium)
                                Text(hit.message.content.compactMap(\.text).joined(separator: "\n")).lineLimit(3)
                            }
                        }
                    }
                }
            } else if let selected {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(model.messages) { message in
                            VStack(alignment: .leading, spacing: 5) {
                                HStack {
                                    Text(message.role == "user" ? "Вы" : message.role == "assistant" ? "Ассистент" : "Система")
                                        .font(.caption.weight(.semibold))
                                    Spacer()
                                    Button("Ответвить…") {
                                        forkOrigin = selected
                                        forkMessage = message
                                        editingChat = nil
                                        title = "\(selected.title) — ответвление"
                                        showChatSheet = true
                                    }
                                    .disabled(model.writesBlocked)
                                    .accessibilityLabel("Ответвить чат с этого сообщения")
                                }
                                ForEach(Array(message.content.enumerated()), id: \.offset) { item in
                                    if let text = item.element.text {
                                        Text(text).textSelection(.enabled)
                                    } else {
                                        Text("Содержимое: \(item.element.type)").foregroundStyle(.secondary)
                                    }
                                }
                                ForEach(message.attachments, id: \.assetId) { attachment in
                                    let path = snapshot.document.assets.first { $0.id == attachment.assetId }?.path ?? attachment.assetId
                                    Label(path, systemImage: "paperclip").font(.caption).textSelection(.enabled)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(10)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: 8))
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                if !selected.archived {
                    let draftKey = "\(snapshot.document.projectId):\(selected.id)"
                    TextEditor(text: Binding(
                        get: { model.chatDrafts[draftKey, default: ""] },
                        set: { model.chatDrafts[draftKey] = $0 }
                    ))
                    .frame(minHeight: 75, maxHeight: 120)
                    .accessibilityLabel("Текст сообщения")
                    .accessibilityIdentifier("chat-message")
                    .disabled(model.writesBlocked)
                    HStack {
                        Picker("Вложение", selection: Binding(
                            get: { model.chatAttachmentDrafts[draftKey, default: ""] },
                            set: { model.chatAttachmentDrafts[draftKey] = $0 }
                        )) {
                            Text("Без вложения").tag("")
                            ForEach(snapshot.document.assets) { asset in
                                Text(asset.path).tag(asset.id)
                            }
                        }
                        Button("Сохранить сообщение") {
                            let text = model.chatDrafts[draftKey, default: ""]
                            let attachment = model.chatAttachmentDrafts[draftKey, default: ""]
                            model.appendMessage(text, attachmentIds: attachment.isEmpty ? [] : [attachment]) {
                                model.chatDrafts[draftKey] = ""
                                model.chatAttachmentDrafts[draftKey] = ""
                            }
                        }
                        .accessibilityIdentifier("save-chat-message")
                        .disabled(model.writesBlocked || (model.chatDrafts[draftKey, default: ""].trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && model.chatAttachmentDrafts[draftKey, default: ""].isEmpty))
                    }
                } else {
                    Text("Чат в архиве. Верните его из архива, чтобы добавлять сообщения.")
                        .foregroundStyle(.secondary)
                }
            } else {
                ContentUnavailableView("Нет открытого чата", systemImage: "bubble.left.and.bubble.right")
            }
        }
        .padding(20)
        .onAppear { model.requestChats(select: model.selectedChatId) }
        .onChange(of: showChatSheet) { _, presented in model.chatEditorPresented = presented }
        .onDisappear { model.chatEditorPresented = false }
        .sheet(isPresented: $showChatSheet) {
            VStack(alignment: .leading, spacing: 16) {
                Text(forkOrigin != nil ? "Ответвить чат" : editingChat == nil ? "Новый чат" : "Переименовать чат").font(.headline)
                TextField("Название", text: $title).accessibilityIdentifier("chat-title").disabled(model.operation != nil)
                if editingChat == nil && forkOrigin == nil {
                    Picker("Режим", selection: $mode) {
                        Text("Обсудить").tag("discuss")
                        Text("План").tag("plan")
                        Text("Изменить").tag("edit")
                    }
                    .disabled(model.operation != nil)
                }
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
                    Button("Отмена") { model.error = nil; showChatSheet = false }.disabled(model.operation != nil)
                    Button("Сохранить") {
                        guard model.operation == nil else { return }
                        model.error = nil
                        if let forkOrigin, let forkMessage {
                            model.forkChat(forkOrigin, through: forkMessage, title: title) { showChatSheet = false }
                        } else if let editingChat {
                            model.renameChat(editingChat, title: title) { showChatSheet = false }
                        } else {
                            model.createChat(title: title, mode: mode) { showChatSheet = false }
                        }
                    }
                    .keyboardShortcut(.defaultAction)
                    .accessibilityIdentifier("confirm-chat")
                    .disabled(model.operation != nil || model.writesBlocked || title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }.padding(24).frame(width: 420)
                .interactiveDismissDisabled(model.operation != nil)
        }
        .alert("Удалить чат?", isPresented: Binding(get: { deleteCandidate != nil }, set: {
            if !$0 { deleteCandidate = nil }
        }), presenting: deleteCandidate) { chat in
            Button("Удалить чат", role: .destructive) {
                guard model.operation == nil else { return }
                model.deleteChat(chat)
                deleteCandidate = nil
            }
            .disabled(model.operation != nil)
            Button("Отмена", role: .cancel) { deleteCandidate = nil }
        } message: { chat in
            Text("Переписка «\(chat.title)» будет удалена из проекта. Применённые изменения дизайна останутся в истории проекта.")
        }
    }
}
