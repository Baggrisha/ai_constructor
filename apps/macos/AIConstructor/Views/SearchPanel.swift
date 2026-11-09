import SwiftUI

/// Project-wide text search and replace (C.5). Matches are previewed before
/// any change; "Заменить все" writes them in one transaction, so a single
/// undo restores every field.
struct SearchPanel: View {
    @ObservedObject var model: WorkspaceModel
    @Binding var isPresented: Bool
    @StoredState private var query = ""
    @StoredState private var replacement = ""

    private var hits: [TextHit] {
        model.searchText(query).sorted { ($0.nodeName, $0.field) < ($1.nodeName, $1.field) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("Поиск и замена").font(.headline)
                Spacer()
                Button {
                    isPresented = false
                    model.commitDraft()
                } label: { Image(systemName: "xmark") }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Закрыть поиск")
            }
            TextField("Найти", text: $query)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("search-query")
            TextField("Заменить на", text: $replacement)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("search-replacement")
            Text(hits.isEmpty ? (query.isEmpty ? "Введите строку" : "Совпадений нет") : "Совпадений: \(hits.count)")
                .font(.caption)
                .foregroundStyle(.secondary)
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(hits) { hit in
                        Button {
                            model.selectNode(hit.nodeId, extending: false)
                        } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("\(hit.nodeName) · \(hit.title)").font(.caption.weight(.medium))
                                Text(hit.snippet).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .padding(.vertical, 2)
                    }
                }
            }
            .frame(maxHeight: 180)
            HStack {
                Spacer()
                Button("Заменить все") {
                    model.replaceAll(query, with: replacement)
                }
                .disabled(hits.isEmpty || model.writesBlocked)
                .accessibilityIdentifier("search-replace-all")
            }
        }
        .padding(14)
        .frame(width: 360)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 12)
        .accessibilityIdentifier("search-panel")
    }
}
