import AppKit
import SwiftUI
import WebKit

/// A read-only scaled preview of a project page: the same Rust-rendered HTML
/// as the canvas, without gestures or overlay.
struct PreviewThumbnail: View {
    let model: WorkspaceModel
    var page: CanvasPage? = nil

    @StoredState private var html: String?
    @StoredState private var loading = true
    @StoredState private var error: String?

    var body: some View {
        ZStack {
            Color(nsColor: .underPageBackgroundColor)
            if let html, !html.isEmpty {
                ReadOnlyPageWebView(html: html)
            } else if let error {
                ContentUnavailableView {
                    Label("Превью недоступно", systemImage: "exclamationmark.triangle")
                } description: {
                    Text(error).font(.caption)
                }
            }
            if loading {
                ProgressView().controlSize(.small)
            }
        }
        .accessibilityIdentifier("preview-thumbnail")
        .task(id: "\(page?.id ?? "first")|\(model.snapshot?.document.revision ?? 0)") {
            loading = true
            error = nil
            let rendered = await model.previewHTML(pageId: page?.id)
            guard !Task.isCancelled else { return }
            html = rendered
            loading = false
            if rendered == nil { error = "Страница не отрисована." }
        }
    }
}

/// Hosts thumbnail HTML: no scripts, no navigation, nothing interactive.
@MainActor
struct ReadOnlyPageWebView: NSViewRepresentable {
    let html: String

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.setValue(false, forKey: "drawsBackground")
        view.allowsMagnification = false
        view.allowsLinkPreview = false
        view.underPageBackgroundColor = .clear
        view.loadHTMLString(html, baseURL: CanvasWebView.baseURL)
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        guard context.coordinator.loaded != html else { return }
        context.coordinator.loaded = html
        view.loadHTMLString(html, baseURL: CanvasWebView.baseURL)
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    @MainActor
    final class Coordinator {
        var loaded: String?
    }
}
