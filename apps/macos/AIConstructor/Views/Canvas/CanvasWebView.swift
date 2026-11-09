import AppKit
import SwiftUI
import WebKit

/// The inspection channel injected into the rendered page. It reports where
/// the pointer is and nothing else: no DOM reads, no edits, no navigation.
enum CanvasBridge {
    static let script = """
    (function () {
      function report(type, event) {
        var target = event.target.closest ? event.target.closest('[data-id]') : null;
        window.webkit.messageHandlers.aic.postMessage({
          type: type,
          id: target ? target.getAttribute('data-id') : null,
          shift: !!event.shiftKey
        });
      }
      document.addEventListener('pointerdown', function (event) {
        if (event.button !== 0) { return; }
        event.preventDefault();
        report('down', event);
      }, true);
      document.addEventListener('pointermove', function (event) { report('move', event); }, true);
      document.addEventListener('contextmenu', function (event) { event.preventDefault(); }, true);
      document.addEventListener('dragstart', function (event) { event.preventDefault(); }, true);
      document.addEventListener('click', function (event) {
        event.preventDefault();
        event.stopPropagation();
      }, true);
      document.addEventListener('mousedown', function (event) { event.preventDefault(); }, true);
      document.addEventListener('keydown', function (event) { event.preventDefault(); }, true);
    })();
    """
}

enum CanvasMessage {
    case hover(String?)
    case down(String?)

    init?(_ body: Any) {
        guard let body = body as? [String: Any], let type = body["type"] as? String else { return nil }
        let id = body["id"] as? String
        switch type {
        case "move": self = .hover(id)
        case "down": self = .down(id)
        default: return nil
        }
    }
}

/// Hosts the Rust-rendered page. Navigation, new windows, external resources
/// and drags are refused; the only channel back to Swift is `CanvasMessage`.
struct CanvasWebView: NSViewRepresentable {
    let html: String
    let scale: Double
    let onMessage: (CanvasMessage) -> Void

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = true
        configuration.defaultWebpagePreferences.preferredContentMode = .desktop
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = false
        configuration.userContentController.add(context.coordinator, name: "aic")
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.setValue(false, forKey: "drawsBackground")
        view.underPageBackgroundColor = .clear
        // The HTML is pre-scaled with a CSS transform; the web view must show
        // it at exactly 1:1 so overlay coordinates match.
        view.allowsMagnification = false
        view.allowsBackForwardNavigationGestures = false
        context.coordinator.view = view
        context.coordinator.onMessage = onMessage
        context.coordinator.loadedHTML = html
        context.coordinator.load(html)
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        context.coordinator.onMessage = onMessage
        guard context.coordinator.loadedHTML != html else { return }
        context.coordinator.loadedHTML = html
        context.coordinator.load(html)
    }

    static func dismantleNSView(_ view: WKWebView, coordinator: Coordinator) {
        view.configuration.userContentController.removeScriptMessageHandler(forName: "aic")
        view.stopLoading()
        view.navigationDelegate = nil
    }

    /// Rendered pages keep their own document directory as base so project
    /// assets resolve locally; nothing is fetched from the network.
    static let baseURL = URL(string: "aic-canvas://page")!

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        weak var view: WKWebView?
        var onMessage: ((CanvasMessage) -> Void)?
        var loadedHTML: String?
        private var generation = 0

        func load(_ html: String) {
            guard let view else { return }
            generation += 1
            // An empty document stands in for "nothing loaded yet"; it is not
            // a project page and never becomes interactive.
            if html.isEmpty {
                view.loadHTMLString("<!DOCTYPE html><html><body></body></html>", baseURL: CanvasWebView.baseURL)
                return
            }
            view.loadHTMLString(html, baseURL: CanvasWebView.baseURL)
        }


        func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            // Only the document itself may load; links, frames and forms are refused.
            let allowed = navigationAction.targetFrame == nil
                ? false
                : navigationAction.navigationType == .other && navigationAction.request.url?.scheme != "https"
                && navigationAction.request.url?.scheme != "http"
            decisionHandler(allowed ? .allow : .cancel)
        }

        func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
            if let html = loadedHTML { load(html) }
        }

        func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
            guard message.name == "aic", let value = CanvasMessage(message.body) else { return }
            onMessage?(value)
        }
    }
}
