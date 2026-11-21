import Darwin
import Foundation
import UIKit
import UniformTypeIdentifiers
import WebKit

/// Native half of the iOS import bridge.
///
/// Rust calls the `@_cdecl` functions at the bottom of this file. The bridge
/// copies picked or opened archives into Application Support/inbox, then queues
/// one JSON payload per request. It mirrors the Android contract:
///
/// `{"requestId":1,"status":"ok|partial|canceled|error","files":[{"name":"a.cbz","path":"/..."}],"failed":[{"name":"b.rar","reason":"..."}]}`
private final class YejianDocumentBridge: NSObject, UIApplicationDelegate, UIDocumentPickerDelegate {
    static let shared = YejianDocumentBridge()

    private let lock = NSLock()
    private var results: [String] = []
    private var nextRequestId: Int64 = 1
    private var activeRequestId: Int64 = 0
    private var activeMode: Int64 = 0
    private weak var activePicker: UIDocumentPickerViewController?
    private var waker: (@convention(c) () -> Void)?
    private let importQueue = DispatchQueue(
        label: "app.yejian.reader.rust.import",
        qos: .userInitiated
    )
    private var prepared = false
    private var scheduled = false
    #if DEBUG
    private var snapshotMarker: UIView?
    #endif

    private override init() {
        super.init()
    }

    // MARK: - Paths

    private var appDataURL: URL {
        let base = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first!
        return base.appendingPathComponent("Yejian", isDirectory: true)
    }

    private var inboxURL: URL {
        appDataURL.appendingPathComponent("inbox", isDirectory: true)
    }

    private var documentsURL: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first!
    }

    // MARK: - Lifecycle

    /// Safe to call before UIApplicationMain. UIKit work is deferred until the
    /// launch notification or the first main-queue turn.
    func prepare() {
        guard !scheduled else { return }
        scheduled = true
        if Thread.isMainThread {
            NotificationCenter.default.addObserver(
                forName: UIApplication.didFinishLaunchingNotification,
                object: nil,
                queue: .main
            ) { [weak self] _ in
                self?.prepareNow()
            }
        }
        DispatchQueue.main.async { [weak self] in
            self?.prepareNow()
        }
    }

    private func prepareNow() {
        guard !prepared else { return }
        prepared = true
        do {
            try FileManager.default.createDirectory(
                at: inboxURL,
                withIntermediateDirectories: true
            )
        } catch {
            // `inboxPath()` returns the path anyway; a real failure is reported
            // by the first import request.
        }

        // winit 0.30 receives lifecycle notifications without installing an
        // application delegate. Supply document-open handling only when the
        // embedding host has left this slot empty; preserve any host delegate.
        if UIApplication.shared.delegate == nil {
            UIApplication.shared.delegate = self
        }
        // Moving large files must not block the first frame; the batch is
        // queued and the waker repaints when it is ready.
        importQueue.async { [weak self] in
            self?.adoptDocumentsInbox()
        }
    }

    // MARK: - Picking

    /// `mode` 0 picks comic archives, 1 picks a `zip`/`json` library backup.
    func pickDocuments(mode: Int64) -> Int64 {
        guard prepared else { return -1 }
        return onMain { () -> Int64 in
            guard self.activePicker == nil,
                  let presenter = self.topViewController() else {
                return -1
            }
            let requestId = self.beginRequest()

            var types: [UTType] = []
            let extensions = mode == 1
                ? ["zip", "json"]
                : ["cbz", "cbr", "cbt", "zip", "rar", "tar", "gz"]
            for ext in extensions {
                if let type = UTType(filenameExtension: ext), !types.contains(type) {
                    types.append(type)
                }
            }
            let picker = UIDocumentPickerViewController(
                forOpeningContentTypes: types.isEmpty ? [.data] : types,
                asCopy: true
            )
            picker.allowsMultipleSelection = true
            picker.delegate = self
            self.activeRequestId = requestId
            self.activeMode = mode
            self.activePicker = picker
            presenter.present(picker, animated: true)
            return requestId
        }
    }

    func documentPicker(
        _ controller: UIDocumentPickerViewController,
        didPickDocumentsAt urls: [URL]
    ) {
        activePicker = nil
        let (requestId, mode) = takeActiveRequest()
        guard requestId > 0 else { return }
        importQueue.async { [weak self] in
            self?.copy(urls: urls, requestId: requestId, mode: mode)
        }
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        activePicker = nil
        let (requestId, mode) = takeActiveRequest()
        guard requestId > 0 else { return }
        enqueue(
            requestId: requestId,
            mode: mode,
            status: "canceled",
            files: [],
            failed: [],
            message: nil
        )
    }

    // MARK: - Document open (warm launch)

    func application(
        _ app: UIApplication,
        open url: URL,
        options: [UIApplication.OpenURLOptionsKey: Any] = [:]
    ) -> Bool {
        let requestId = onMain { self.beginRequest() }
        importQueue.async { [weak self] in
            self?.copy(urls: [url], requestId: requestId, mode: 0)
        }
        return true
    }

    // MARK: - Copying

    private func copy(urls: [URL], requestId: Int64, mode: Int64) {
        let manager = FileManager.default
        do {
            try manager.createDirectory(
                at: inboxURL,
                withIntermediateDirectories: true
            )
        } catch {
            enqueue(
                requestId: requestId,
                mode: mode,
                status: "error",
                files: [],
                failed: [],
                message: "无法创建导入目录：\(error.localizedDescription)"
            )
            return
        }

        var files: [[String: String]] = []
        var failed: [[String: String]] = []
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer {
                if scoped {
                    url.stopAccessingSecurityScopedResource()
                }
            }

            let temporary = inboxURL.appendingPathComponent(
                ".import-\(UUID().uuidString).part"
            )
            do {
                try manager.copyItem(at: url, to: temporary)
                let target = uniqueURL(for: sanitize(url.lastPathComponent))
                try manager.moveItem(at: temporary, to: target)
                files.append([
                    "name": target.lastPathComponent,
                    "path": target.path,
                ])
            } catch {
                try? manager.removeItem(at: temporary)
                failed.append([
                    "name": url.lastPathComponent,
                    "reason": error.localizedDescription,
                ])
            }
        }

        let status: String
        if failed.isEmpty {
            status = "ok"
        } else if files.isEmpty {
            status = "error"
        } else {
            status = "partial"
        }
        enqueue(
            requestId: requestId,
            mode: mode,
            status: status,
            files: files,
            failed: failed,
            message: nil
        )
    }

    /// Files copied by "Copy to 页间" land in Documents/Inbox before launch.
    private func adoptDocumentsInbox() {
        let manager = FileManager.default
        let systemInbox = documentsURL.appendingPathComponent("Inbox", isDirectory: true)

        var files: [[String: String]] = []
        var failed: [[String: String]] = []

        var sources: [URL] = []
        if let entries = try? manager.contentsOfDirectory(
            at: systemInbox,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) {
            sources.append(contentsOf: entries)
        }
        // UIFileSharingEnabled exposes Documents in Finder/Files. Pick up
        // archives the user dropped there directly, but leave unrelated files
        // alone.
        if let entries = try? manager.contentsOfDirectory(
            at: documentsURL,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        ) {
            sources.append(contentsOf: entries.filter {
                isArchive($0.pathExtension)
            })
        }

        for source in sources {
            do {
                let target = uniqueURL(for: sanitize(source.lastPathComponent))
                try manager.moveItem(at: source, to: target)
                files.append([
                    "name": target.lastPathComponent,
                    "path": target.path,
                ])
            } catch {
                failed.append([
                    "name": source.lastPathComponent,
                    "reason": error.localizedDescription,
                ])
            }
        }
        guard !files.isEmpty || !failed.isEmpty else { return }
        let status: String
        if failed.isEmpty {
            status = "ok"
        } else if files.isEmpty {
            status = "error"
        } else {
            status = "partial"
        }
        enqueue(
            requestId: 0,
            mode: 0,
            status: status,
            files: files,
            failed: failed,
            message: "Documents/Inbox"
        )
    }

    private func isArchive(_ pathExtension: String) -> Bool {
        switch pathExtension.lowercased() {
        case "cbz", "cbr", "cbt", "zip", "rar", "tar", "gz":
            return true
        default:
            return false
        }
    }

    private func uniqueURL(for name: String) -> URL {
        let manager = FileManager.default
        let nsName = name as NSString
        let base = nsName.deletingPathExtension
        let ext = nsName.pathExtension
        var candidate = name
        var index = 2
        while manager.fileExists(
            atPath: inboxURL.appendingPathComponent(candidate).path
        ) {
            candidate = ext.isEmpty
                ? "\(base) (\(index))"
                : "\(base) (\(index)).\(ext)"
            index += 1
        }
        return inboxURL.appendingPathComponent(candidate)
    }

    private func sanitize(_ raw: String) -> String {
        var name = raw
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "\\", with: "_")
            .trimmingCharacters(in: .whitespacesAndNewlines)
        if name.isEmpty || name == "." || name == ".." {
            name = "import-\(Int(Date().timeIntervalSince1970))"
        }
        if name.count > 180 {
            let nsName = name as NSString
            let ext = nsName.pathExtension
            let base = nsName.deletingPathExtension
            let limit = max(1, 180 - (ext.isEmpty ? 0 : ext.count + 1))
            name = String(base.prefix(limit)) + (ext.isEmpty ? "" : ".\(ext)")
        }
        return name
    }

    // MARK: - Results

    private func beginRequest() -> Int64 {
        let requestId = nextRequestId
        nextRequestId += 1
        return requestId
    }

    private func takeActiveRequest() -> (Int64, Int64) {
        let requestId = activeRequestId
        let mode = activeMode
        activeRequestId = 0
        activeMode = 0
        return (requestId, mode)
    }

    private func enqueue(
        requestId: Int64,
        mode: Int64,
        status: String,
        files: [[String: String]],
        failed: [[String: String]],
        message: String?
    ) {
        var payload: [String: Any] = [
            "requestId": requestId,
            "kind": mode == 1 ? "backup" : "comics",
            "status": status,
            "files": files,
            "failed": failed,
        ]
        if let message {
            payload["message"] = message
        }
        guard let data = try? JSONSerialization.data(withJSONObject: payload),
              let json = String(data: data, encoding: .utf8) else {
            return
        }
        lock.lock()
        results.append(json)
        lock.unlock()
        wake()
    }

    func poll() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return results.isEmpty ? nil : results.removeFirst()
    }

    // MARK: - Waking Rust

    func setWaker(_ callback: @escaping @convention(c) () -> Void) {
        lock.lock()
        waker = callback
        lock.unlock()
    }

    private func wake() {
        lock.lock()
        let callback = waker
        lock.unlock()
        guard let callback else { return }
        // UIKit/egui both want the main thread; hop when a copy finished off it.
        if Thread.isMainThread {
            callback()
        } else {
            DispatchQueue.main.async { callback() }
        }
    }

    // MARK: - Inputs from Rust

    func inboxPath() -> String {
        try? FileManager.default.createDirectory(
            at: inboxURL,
            withIntermediateDirectories: true
        )
        return inboxURL.path
    }

    func appDataPath() -> String {
        try? FileManager.default.createDirectory(
            at: appDataURL,
            withIntermediateDirectories: true
        )
        return appDataURL.path
    }

    func safeInsets() -> String {
        guard prepared else { return "0,0,0,0" }
        return onMain {
            guard let window = self.keyWindow() else { return "0,0,0,0" }
            let insets = window.safeAreaInsets
            let scale = window.screen.scale
            return [insets.left, insets.top, insets.right, insets.bottom]
                .map { String(Int(($0 * scale).rounded())) }
                .joined(separator: ",")
        }
    }

    /// Presents the system share sheet for an app-private file. Returns 0 when
    /// the sheet was presented, -1 without a presenter, -2 when the file is
    /// missing.
    func shareFile(path: String) -> Int64 {
        guard prepared else { return -1 }
        let url = URL(fileURLWithPath: path)
        guard FileManager.default.fileExists(atPath: url.path) else { return -2 }
        return onMain {
            guard let presenter = self.topViewController() else { return -1 }
            let controller = UIActivityViewController(
                activityItems: [url],
                applicationActivities: nil
            )
            if let popover = controller.popoverPresentationController {
                popover.sourceView = presenter.view
                popover.sourceRect = CGRect(
                    x: presenter.view.bounds.midX,
                    y: presenter.view.bounds.midY,
                    width: 1,
                    height: 1
                )
            }
            presenter.present(controller, animated: true)
            return 0
        }
    }

    // MARK: - UIKit helpers

    #if DEBUG
    /// Test-only acknowledgement from the Rust renderer. This transparent
    /// accessibility element carries its actual readiness report to XCTest.
    /// Release builds contain neither this element nor the exported callback.
    func snapshotReady(_ json: String) {
        onMain {
            guard let window = self.keyWindow() else { return }
            let marker = self.snapshotMarker ?? UIView()
            marker.frame = CGRect(
                x: window.safeAreaInsets.left + 1,
                y: window.safeAreaInsets.top + 1,
                width: 2,
                height: 2
            )
            marker.isUserInteractionEnabled = false
            marker.isAccessibilityElement = true
            marker.accessibilityIdentifier = "yejian.snapshot.ready"
            marker.accessibilityLabel = "Rendered screen ready"
            marker.accessibilityValue = json
            if marker.superview == nil { window.addSubview(marker) }
            self.snapshotMarker = marker
        }
    }
    #endif

    private func keyWindow() -> UIWindow? {
        for scene in UIApplication.shared.connectedScenes {
            guard let windowScene = scene as? UIWindowScene else { continue }
            if let key = windowScene.keyWindow {
                return key
            }
            if let first = windowScene.windows.first {
                return first
            }
        }
        return UIApplication.shared.windows.first
    }

    private func topViewController() -> UIViewController? {
        var controller = keyWindow()?.rootViewController
        while let presented = controller?.presentedViewController {
            controller = presented
        }
        return controller
    }

    @discardableResult
    private func onMain<T>(_ body: () -> T) -> T {
        if Thread.isMainThread {
            return body()
        }
        return DispatchQueue.main.sync(execute: body)
    }
}

// MARK: - In-app browser (Cloudflare verification, web login)

private let webLock = NSLock()
private var webResults: [String] = []

/// Full-screen web page; "完成" hands the page's cookies back to Rust.
private final class YejianWebController: UIViewController, WKNavigationDelegate {
    private let url: URL
    private let agent: String
    private let finish: (String) -> Void
    private let webView: WKWebView
    private var sent = false

    init(url: URL, agent: String, finish: @escaping (String) -> Void) {
        self.url = url
        self.agent = agent
        self.finish = finish
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .default()
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { nil }

    override func loadView() {
        view = webView
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        webView.customUserAgent = agent
        webView.navigationDelegate = self
        navigationItem.title = "完成验证或登录后点「完成」"
        navigationItem.leftBarButtonItem = UIBarButtonItem(
            barButtonSystemItem: .cancel, target: self, action: #selector(cancel))
        navigationItem.rightBarButtonItem = UIBarButtonItem(
            title: "完成", style: .done, target: self, action: #selector(done))
        webView.load(URLRequest(url: url))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if let title = webView.title, !title.isEmpty {
            navigationItem.title = title
        }
    }

    @objc private func cancel() {
        send(cookies: nil)
    }

    @objc private func done() {
        let host = url.host ?? ""
        webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { cookies in
            let picked = cookies.filter { cookie in
                let domain = cookie.domain.hasPrefix(".") ? String(cookie.domain.dropFirst()) : cookie.domain
                return host == domain || host.hasSuffix("." + domain)
            }
            self.send(cookies: picked.map { ["name": $0.name, "value": $0.value] })
        }
    }

    private func send(cookies: [[String: String]]?) {
        guard !sent else { return }
        sent = true
        var payload: [String: Any] = ["url": url.absoluteString]
        if let cookies { payload["cookies"] = cookies }
        let data = (try? JSONSerialization.data(withJSONObject: payload)) ?? Data()
        finish(String(data: data, encoding: .utf8) ?? "")
        dismiss(animated: true)
    }
}

extension YejianDocumentBridge {
    /// Returns 0 when the page was presented, -1 without a presenter or URL.
    func openWeb(url: String, agent: String) -> Int64 {
        guard prepared, let target = URL(string: url) else { return -1 }
        return onMain {
            guard let presenter = self.topViewController() else { return -1 }
            let page = YejianWebController(url: target, agent: agent) { json in
                webLock.lock()
                webResults.append(json)
                webLock.unlock()
                self.wake()
            }
            let navigation = UINavigationController(rootViewController: page)
            navigation.modalPresentationStyle = .fullScreen
            presenter.present(navigation, animated: true)
            return 0
        }
    }

    func pollWeb() -> String? {
        webLock.lock()
        defer { webLock.unlock() }
        return webResults.isEmpty ? nil : webResults.removeFirst()
    }

    /// Hands `url` to `UIApplication.open` (Safari etc.). The Rust caller
    /// already restricted it to http/https. Returns 0 when the system
    /// accepted the request, -1 when it could not be opened.
    func openExternalUrl(url: String) -> Int64 {
        guard prepared, let target = URL(string: url),
              let scheme = target.scheme?.lowercased(),
              scheme == "http" || scheme == "https" else { return -1 }
        return onMain {
            guard UIApplication.shared.canOpenURL(target) else { return -1 }
            UIApplication.shared.open(target, options: [:])
            return 0
        }
    }
}

// MARK: - Rust ABI

#if DEBUG
@_cdecl("yejian_ios_snapshot_ready")
public func yejian_ios_snapshot_ready(_ json: UnsafePointer<CChar>?) {
    guard let json else { return }
    YejianDocumentBridge.shared.snapshotReady(String(cString: json))
}
#endif

@_cdecl("yejian_ios_init")
public func yejian_ios_init() {
    YejianDocumentBridge.shared.prepare()
}

@_cdecl("yejian_ios_pick_documents")
public func yejian_ios_pick_documents(_ mode: Int64) -> Int64 {
    YejianDocumentBridge.shared.pickDocuments(mode: mode)
}

@_cdecl("yejian_ios_set_waker")
public func yejian_ios_set_waker(_ callback: (@convention(c) () -> Void)?) {
    guard let callback else { return }
    YejianDocumentBridge.shared.setWaker(callback)
}

@_cdecl("yejian_ios_share_file")
public func yejian_ios_share_file(_ path: UnsafePointer<CChar>?) -> Int64 {
    guard let path else { return -2 }
    return YejianDocumentBridge.shared.shareFile(path: String(cString: path))
}

@_cdecl("yejian_ios_poll_import")
public func yejian_ios_poll_import() -> UnsafeMutablePointer<CChar>? {
    guard let json = YejianDocumentBridge.shared.poll() else { return nil }
    return duplicate(json)
}

@_cdecl("yejian_ios_free_string")
public func yejian_ios_free_string(_ pointer: UnsafeMutablePointer<CChar>?) {
    free(pointer)
}

@_cdecl("yejian_ios_inbox_dir")
public func yejian_ios_inbox_dir() -> UnsafeMutablePointer<CChar>? {
    duplicate(YejianDocumentBridge.shared.inboxPath())
}

@_cdecl("yejian_ios_app_data_dir")
public func yejian_ios_app_data_dir() -> UnsafeMutablePointer<CChar>? {
    duplicate(YejianDocumentBridge.shared.appDataPath())
}

@_cdecl("yejian_ios_safe_insets")
public func yejian_ios_safe_insets() -> UnsafeMutablePointer<CChar>? {
    duplicate(YejianDocumentBridge.shared.safeInsets())
}

private func duplicate(_ value: String) -> UnsafeMutablePointer<CChar>? {
    value.withCString { strdup($0) }
}

@_cdecl("yejian_ios_open_web")
public func yejian_ios_open_web(
    _ url: UnsafePointer<CChar>?, _ agent: UnsafePointer<CChar>?
) -> Int64 {
    guard let url, let agent else { return -1 }
    return YejianDocumentBridge.shared.openWeb(url: String(cString: url), agent: String(cString: agent))
}

@_cdecl("yejian_ios_poll_web")
public func yejian_ios_poll_web() -> UnsafeMutablePointer<CChar>? {
    guard let json = YejianDocumentBridge.shared.pollWeb() else { return nil }
    return duplicate(json)
}

@_cdecl("yejian_ios_open_external_url")
public func yejian_ios_open_external_url(_ url: UnsafePointer<CChar>?) -> Int64 {
    guard let url else { return -1 }
    return YejianDocumentBridge.shared.openExternalUrl(url: String(cString: url))
}
