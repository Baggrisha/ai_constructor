import Foundation

/// Pending inspector input for one node. Values are keyed by the setProperty
/// path (`/style/fill`, `/layout/x`, …) and held as JSON values.
struct InspectorDraft: Equatable {
    let nodeId: String
    let baseRevision: UInt64
    let baseFingerprint: String
    private(set) var values: [String: Any] = [:]

    init(nodeId: String, baseRevision: UInt64, baseFingerprint: String) {
        self.nodeId = nodeId
        self.baseRevision = baseRevision
        self.baseFingerprint = baseFingerprint
    }

    var isEmpty: Bool { values.isEmpty }

    /// `nil` clears the property (it is sent as JSON null).
    mutating func set(_ path: String, _ value: Any?) {
        if let value {
            values[path] = value
        } else {
            values[path] = NSNull()
        }
    }

    func value(_ path: String) -> Any? {
        guard let value = values[path] else { return nil }
        return value is NSNull ? nil : value
    }

    static func == (lhs: InspectorDraft, rhs: InspectorDraft) -> Bool {
        lhs.nodeId == rhs.nodeId
            && lhs.baseRevision == rhs.baseRevision
            && lhs.baseFingerprint == rhs.baseFingerprint
            && (try? JSONSerialization.data(withJSONObject: lhs.values, options: [.sortedKeys]))
                == (try? JSONSerialization.data(withJSONObject: rhs.values, options: [.sortedKeys]))
    }
}

extension CanvasPatch {
    /// Maps an inspector value at a setProperty path to the CSS change the
    /// loaded page can show immediately. Paths the page cannot preview map to
    /// `nil`: they stay pending and reach the document on commit only.
    static func css(for path: String, value: Any?) -> (key: String, value: String?)? {
        let number = CanvasPatch.cssNumber(value)
        let string = value as? String
        let color = CanvasPatch.trimmed(value)
        let px = number.map { $0 + "px" }
        switch path {
        case "/style/fill":
            return ("background-color", color)
        case "/style/color":
            return ("color", color)
        case "/style/opacity":
            return ("opacity", number)
        case "/style/fontFamily":
            let family = (string ?? "").replacingOccurrences(of: "\"", with: "").trimmingCharacters(in: .whitespaces)
            return ("font-family", family.isEmpty ? nil : "\"\(family)\", sans-serif")
        case "/style/fontSize":
            return ("font-size", px)
        case "/style/fontWeight":
            return ("font-weight", number)
        case "/style/fontStyle":
            return ("font-style", string)
        case "/style/lineHeight":
            return ("line-height", px)
        case "/style/letterSpacing":
            return ("letter-spacing", px)
        case "/style/textAlign":
            return ("text-align", string)
        case "/style/rotate":
            return ("transform", number.map { "rotate(\($0)deg)" })
        case "/style/radius":
            return ("border-radius", px)
        case "/style/stroke":
            guard let stroke = value as? [String: Any], let width = CanvasPatch.cssNumber(stroke["width"]),
                  let strokeColor = CanvasPatch.trimmed(stroke["color"]) else {
                return ("border", nil)
            }
            return ("border", "\(width)px solid \(strokeColor)")
        case "/style/shadows":
            let shadows = (value as? [[String: Any]]) ?? []
            let parts = shadows.compactMap { shadow -> String? in
                guard let shadowColor = CanvasPatch.trimmed(shadow["color"]),
                      let x = CanvasPatch.cssNumber(shadow["x"]), let y = CanvasPatch.cssNumber(shadow["y"]),
                      let blur = CanvasPatch.cssNumber(shadow["blur"]), let spread = CanvasPatch.cssNumber(shadow["spread"]) else {
                    return nil
                }
                return "\(x)px \(y)px \(blur)px \(spread)px \(shadowColor)"
            }
            return ("box-shadow", parts.isEmpty ? nil : parts.joined(separator: ", "))
        case "/style/fit":
            return ("object-fit", string)
        case "/style/overflow":
            return ("overflow", string)
        case "/hidden":
            let hidden = InspectorJSON.flag(value) ?? false
            return ("display", hidden ? "none" : "")
        case "/layout/x":
            return ("left", px)
        case "/layout/y":
            return ("top", px)
        case "/layout/w":
            return ("width", px)
        case "/layout/h":
            return ("height", px)
        case "/content/text", "/content/placeholder":
            return ("text", string ?? number ?? "")
        case "/style/textWrap":
            guard let wrap = string else { return ("white-space", nil) }
            return ("white-space", wrap == "none" ? "pre" : "pre-wrap")
        case "/style/maxLines":
            return ("-webkit-line-clamp", number)
        default:
            return nil
        }
    }

    private static func cssNumber(_ value: Any?) -> String? {
        let number: Double
        if let value = value as? NSNumber {
            number = value.doubleValue
        } else if let value = value as? String, let parsed = Double(value.replacingOccurrences(of: ",", with: ".")) {
            number = parsed
        } else {
            return nil
        }
        guard number.isFinite else { return nil }
        if number.rounded() == number && abs(number) < 1e15 { return String(format: "%.0f", number) }
        return String(number)
    }

    private static func trimmed(_ value: Any?) -> String? {
        guard let value = value as? String else { return nil }
        let trimmed = value.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

/// One replaceable text occurrence of the project.
struct TextHit: Identifiable, Equatable {
    let nodeId: String
    let nodeName: String
    let field: String
    let snippet: String

    var id: String { "\(nodeId)|\(field)" }

    /// The setProperty path the field writes to.
    var path: String {
        switch field {
        case "placeholder": return "/content/placeholder"
        case "a11y.label": return "/a11y/label"
        case "a11y.alt": return "/a11y/alt"
        default: return "/content/text"
        }
    }

    var title: String {
        switch field {
        case "placeholder": return "Placeholder"
        case "a11y.label": return "Метка"
        case "a11y.alt": return "Alt"
        default: return "Текст"
        }
    }
}

enum InsertedNodeTemplate {
    struct Template {
        let kind: String
        let name: String
        let width: Double
        let height: Double
        let style: [String: Any]
        let content: [String: Any]
        let flow: Bool
    }

    static let kinds = ["frame", "text", "image", "vector", "button", "input", "textarea", "checkbox", "select"]

    static func template(_ kind: String) -> Template? {
        switch kind {
        case "frame": return Template(kind: kind, name: "Фрейм", width: 320, height: 240, style: [:], content: [:], flow: true)
        case "text": return Template(kind: kind, name: "Текст", width: 240, height: 40, style: ["fontSize": 16, "fontWeight": 400], content: ["text": "Текст"], flow: false)
        case "image": return Template(kind: kind, name: "Изображение", width: 240, height: 160, style: ["fill": "#E5E7EB", "fit": "cover"], content: [:], flow: false)
        case "vector": return Template(kind: kind, name: "Вектор", width: 120, height: 120, style: ["fill": "#94A3B8"], content: ["vector": ["paths": [], "shapes": [["type": "rect", "x": 0, "y": 0, "w": 120, "h": 120]]]], flow: false)
        case "button": return Template(kind: kind, name: "Кнопка", width: 160, height: 44, style: ["fill": "#2563EB", "color": "#FFFFFF", "fontSize": 15, "fontWeight": 500, "radius": 8], content: ["text": "Кнопка"], flow: false)
        case "input": return Template(kind: kind, name: "Поле ввода", width: 240, height: 44, style: ["fill": "#FFFFFF", "color": "#111827", "fontSize": 14, "radius": 6, "stroke": ["color": "#D1D5DB", "width": 1]], content: ["placeholder": "Введите текст"], flow: false)
        case "textarea": return Template(kind: kind, name: "Многострочное поле", width: 240, height: 120, style: ["fill": "#FFFFFF", "color": "#111827", "fontSize": 14, "radius": 6, "stroke": ["color": "#D1D5DB", "width": 1]], content: ["placeholder": "Введите текст"], flow: false)
        case "checkbox": return Template(kind: kind, name: "Флажок", width: 160, height: 24, style: ["fill": "#2563EB", "radius": 4], content: ["text": "Флажок"], flow: false)
        case "select": return Template(kind: kind, name: "Список", width: 240, height: 44, style: ["fill": "#FFFFFF", "color": "#111827", "fontSize": 14, "radius": 6, "stroke": ["color": "#D1D5DB", "width": 1]], content: ["placeholder": "Выберите значение"], flow: false)
        default: return nil
        }
    }
}

/// Reads properties out of the raw document JSON by their setProperty path.
enum InspectorJSON {
    /// `style`/`content`/`a11y`/… are stored as `{"version": 2, "value": …}`.
    static func feature(_ value: Any?) -> Any? {
        guard let object = value as? [String: Any] else { return value }
        if object.isEmpty { return nil }
        return object["value"] ?? object
    }

    /// The value at `path` ("/style/fill", "/layout/x", …), or nil when the
    /// property is absent or unreadable.
    static func value(in node: [String: Any], path: String) -> Any? {
        let parts = path.split(separator: "/").map(String.init).filter { !$0.isEmpty }
        guard let root = parts.first else { return nil }
        var current = feature(node[root])
        for part in parts.dropFirst() {
            guard let object = current as? [String: Any] else { return nil }
            current = feature(object[part])
        }
        return current is NSNull ? nil : current
    }

    /// JSON numbers arrive as `NSNumber`, which the `Double` cast rejects.
    static func number(_ value: Any?) -> Double? {
        (value as? NSNumber)?.doubleValue
    }

    static func flag(_ value: Any?) -> Bool? {
        (value as? NSNumber)?.boolValue
    }

    /// Loose comparison: 16 equals 16.0, absent equals null or "".
    static func equal(_ a: Any, _ b: Any) -> Bool {
        let aNull = a is NSNull || ((a as? String)?.isEmpty ?? false)
        let bNull = b is NSNull || ((b as? String)?.isEmpty ?? false)
        if aNull || bNull { return aNull && bNull }
        if let a = a as? NSNumber, let b = b as? NSNumber { return a.doubleValue == b.doubleValue }
        if let a = a as? String, let b = b as? String { return a == b }
        if let a = a as? Bool, let b = b as? NSNumber { return a == b.boolValue }
        let encoder = { (value: Any) -> Data? in try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
        if let a = encoder(a), let b = encoder(b) { return a == b }
        return false
    }
}

struct LayerRow: Identifiable {
    let id: String
    let name: String
    let kind: String
    let depth: Int
    let hasChildren: Bool
    let hidden: Bool
    let locked: Bool

    var icon: String {
        switch kind {
        case "frame": return "square.dashed"
        case "group": return "square.on.square"
        case "text": return "textformat"
        case "image": return "photo"
        case "vector": return "scribble"
        case "button": return "capsule"
        case "input", "textarea": return "rectangle.and.pencil.and.ellipsis"
        case "checkbox": return "checkmark.square"
        case "select": return "chevron.up.chevron.down"
        case "componentInstance": return "square.stack.3d.up"
        case "codeComponent": return "curlybraces"
        default: return "questionmark.square.dashed"
        }
    }
}
