import Darwin
import Foundation
import BackgroundTasks
import LocalAuthentication
import UIKit
import UserNotifications
import UniformTypeIdentifiers
import WebKit

/// Native half of the iOS import bridge.
///
/// Rust calls the `@_cdecl` functions at the bottom of this file. The bridge
/// copies picked or opened archives into Application Support/inbox, then queues
/// one JSON payload per request. It mirrors the Android contract:
///
/// `{"requestId":1,"status":"ok|partial|canceled|error","files":[{"name":"a.cbz","path":"/..."}],"failed":[{"name":"b.rar","reason":"..."}]}`
private final class YejianDocumentBridge: NSObject, UIApplicationDelegate, UIDocumentPickerDelegate, UNUserNotificationCenterDelegate {
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
    private var authResults: [String] = []
    private var foregroundEvents: [String] = []
    /// Notification permission results ("1"/"0") and open payloads.
    private var notifyPermResults: [String] = []
    private var notifyOpens: [String] = []
    /// Seconds the next BGAppRefreshTaskRequest waits at least (default 1 h).
    private var refreshIntervalSeconds: TimeInterval = 3600
    /// Rust-owned background check plus its string free function.
    private var bgRefreshHandler: (@convention(c) () -> UnsafeMutablePointer<CChar>?)?
    private var bgRefreshFree: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void)?
    private static let refreshTaskId = "app.yejian.reader.rust.refresh"

    private var shieldWanted = false
    private weak var shieldView: UIView?
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

        // Lifecycle for the app lock: background/foreground reach Rust as a
        // pollable queue, and the privacy shield hides the snapshot the
        // system keeps for the app switcher while app lock is enabled.
        NotificationCenter.default.addObserver(
            forName: UIApplication.didEnterBackgroundNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.enqueueForeground("b")
            self?.scheduleBackgroundRefresh()
        }
        NotificationCenter.default.addObserver(
            forName: UIApplication.willEnterForegroundNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.enqueueForeground("f")
        }
        NotificationCenter.default.addObserver(
            forName: UIApplication.willResignActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.showShield()
        }
        NotificationCenter.default.addObserver(
            forName: UIApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.hideShield()
        }

        // 新章节通知: the delegate routes taps to Rust, and the refresh task
        // is registered while didFinishLaunching is still on the stack.
        UNUserNotificationCenter.current().delegate = self
        BGTaskScheduler.shared.register(
            forTaskWithIdentifier: Self.refreshTaskId,
            using: nil
        ) { [weak self] task in
            if let task = task as? BGAppRefreshTask {
                self?.handleRefresh(task)
            }
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

    // MARK: - App lock (LocalAuthentication + privacy shield)

    /// `LAContext.canEvaluatePolicy(.deviceOwnerAuthentication)`.
    func supportsAppLock() -> Int64 {
        LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) ? 1 : 0
    }

    /// Face/Touch ID or the device passcode. 0 = prompt started, -2 = the
    /// device cannot authenticate, -1 = not prepared.
    func authenticate(reason: String) -> Int64 {
        guard prepared else { return -1 }
        let context = LAContext()
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            return -2
        }
        context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) {
            [weak self] ok, authError in
            let message = ok ? "ok" : (authError?.localizedDescription ?? "canceled")
            self?.enqueueAuthResult(message)
        }
        return 0
    }

    private func enqueueAuthResult(_ message: String) {
        lock.lock()
        authResults.append(message)
        lock.unlock()
        wake()
    }

    func pollAuth() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return authResults.isEmpty ? nil : authResults.removeFirst()
    }

    private func enqueueForeground(_ event: String) {
        lock.lock()
        foregroundEvents.append(event)
        lock.unlock()
        wake()
    }

    func pollForeground() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return foregroundEvents.isEmpty ? nil : foregroundEvents.removeFirst()
    }

    /// Rust toggles this with the 应用锁 setting.
    func setPrivacyShield(_ on: Bool) {
        shieldWanted = on
    }

    /// Opaque paper-colored cover for the app-switcher snapshot.
    private func showShield() {
        guard shieldWanted, shieldView == nil, let window = keyWindow() else { return }
        let view = UIView(frame: window.bounds)
        view.backgroundColor = UIColor(red: 1.0, green: 0.992, blue: 0.976, alpha: 1.0)
        view.isUserInteractionEnabled = false
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        window.addSubview(view)
        shieldView = view
    }

    private func hideShield() {
        shieldView?.removeFromSuperview()
        shieldView = nil
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

    // MARK: - Notifications

    /// UNUserNotificationCenter permission request; result queued as "1"/"0".
    func requestNotificationPermission() {
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .badge, .sound]
        ) { [weak self] granted, _ in
            self?.enqueueNotifyPerm(granted ? "1" : "0")
        }
    }

    private func enqueueNotifyPerm(_ value: String) {
        lock.lock()
        notifyPermResults.append(value)
        lock.unlock()
        wake()
    }

    func pollNotificationPermission() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return notifyPermResults.isEmpty ? nil : notifyPermResults.removeFirst()
    }

    /// Immediate UNNotificationRequest; the payload rides in userInfo.
    func postNotification(id: Int64, title: String, body: String, payload: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        content.userInfo = ["payload": payload]
        let request = UNNotificationRequest(
            identifier: "yejian-chapters-\(id)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    func takeNotificationOpen() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return notifyOpens.isEmpty ? nil : notifyOpens.removeFirst()
    }

    /// Routes a notification tap to Rust. No banner while foregrounded — the
    /// in-app notice already covers fresh chapters.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        if let payload = response.notification.request.content.userInfo["payload"] as? String {
            lock.lock()
            notifyOpens.append(payload)
            lock.unlock()
            wake()
        }
        completionHandler()
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler:
            @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([])
    }

    /// How long the next refresh request waits at least.
    func setRefreshInterval(_ seconds: UInt64) {
        lock.lock()
        refreshIntervalSeconds = TimeInterval(max(60, seconds))
        lock.unlock()
    }

    /// Rust's check function, wired through the handler setter below.
    func setBackgroundRefreshHandler(
        _ handler: (@convention(c) () -> UnsafeMutablePointer<CChar>?)?,
        free: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void)?
    ) {
        lock.lock()
        bgRefreshHandler = handler
        bgRefreshFree = free
        lock.unlock()
    }

    /// Schedules the next BGAppRefreshTask on entering background.
    private func scheduleBackgroundRefresh() {
        lock.lock()
        let interval = refreshIntervalSeconds
        lock.unlock()
        let request = BGAppRefreshTaskRequest(identifier: Self.refreshTaskId)
        request.earliestBeginDate = Date(timeIntervalSinceNow: interval)
        try? BGTaskScheduler.shared.submit(request)
    }

    /// Runs Rust's check off the main queue, posts each notification it
    /// returns, then schedules the next refresh.
    private func handleRefresh(_ task: BGAppRefreshTask) {
        task.expirationHandler = {
            task.setTaskCompleted(success: false)
        }
        DispatchQueue.global().async { [weak self] in
            defer { task.setTaskCompleted(success: true) }
            guard let self = self else { return }
            self.lock.lock()
            let handler = self.bgRefreshHandler
            let free = self.bgRefreshFree
            self.lock.unlock()
            guard let handler, let pointer = handler() else { return }
            let json = String(cString: pointer)
            free?(pointer)
            guard let data = json.data(using: .utf8),
                  let notes = try? JSONSerialization.jsonObject(with: data) as? [[String: Any]]
            else {
                return
            }
            let center = UNUserNotificationCenter.current()
            for note in notes {
                guard let id = note["id"] as? Int,
                      let title = note["title"] as? String,
                      let body = note["body"] as? String,
                      let payload = note["payload"] as? String
                else {
                    continue
                }
                let content = UNMutableNotificationContent()
                content.title = title
                content.body = body
                content.sound = .default
                content.userInfo = ["payload": payload]
                center.add(UNNotificationRequest(
                    identifier: "yejian-chapters-\(id)",
                    content: content,
                    trigger: nil
                ))
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

@_cdecl("yejian_ios_set_keep_awake")
public func yejian_ios_set_keep_awake(_ awake: Bool) {
    DispatchQueue.main.async {
        UIApplication.shared.isIdleTimerDisabled = awake
    }
}

/// Battery level in percent, or -1 when the device cannot report one.
/// Monitoring is enabled on the first query.
@_cdecl("yejian_ios_battery_level")
public func yejian_ios_battery_level() -> Float {
    if !Thread.isMainThread {
        return DispatchQueue.main.sync { yejian_ios_battery_level() }
    }
    let device = UIDevice.current
    if !device.isBatteryMonitoringEnabled {
        device.isBatteryMonitoringEnabled = true
    }
    let level = device.batteryLevel
    return level < 0 ? -1 : Float(level * 100)
}

@_cdecl("yejian_ios_open_external_url")
public func yejian_ios_open_external_url(_ url: UnsafePointer<CChar>?) -> Int64 {
    guard let url else { return -1 }
    return YejianDocumentBridge.shared.openExternalUrl(url: String(cString: url))
}

@_cdecl("yejian_ios_supports_app_lock")
public func yejian_ios_supports_app_lock() -> Int64 {
    YejianDocumentBridge.shared.supportsAppLock()
}

@_cdecl("yejian_ios_authenticate")
public func yejian_ios_authenticate(_ reason: UnsafePointer<CChar>?) -> Int64 {
    guard let reason else { return -1 }
    return YejianDocumentBridge.shared.authenticate(reason: String(cString: reason))
}

@_cdecl("yejian_ios_poll_auth")
public func yejian_ios_poll_auth() -> UnsafeMutablePointer<CChar>? {
    guard let result = YejianDocumentBridge.shared.pollAuth() else { return nil }
    return duplicate(result)
}

@_cdecl("yejian_ios_poll_foreground")
public func yejian_ios_poll_foreground() -> UnsafeMutablePointer<CChar>? {
    guard let event = YejianDocumentBridge.shared.pollForeground() else { return nil }
    return duplicate(event)
}

@_cdecl("yejian_ios_set_privacy_shield")
public func yejian_ios_set_privacy_shield(_ on: Bool) {
    YejianDocumentBridge.shared.setPrivacyShield(on)
}

@_cdecl("yejian_ios_request_notification_permission")
public func yejian_ios_request_notification_permission() {
    YejianDocumentBridge.shared.requestNotificationPermission()
}

@_cdecl("yejian_ios_poll_notification_permission")
public func yejian_ios_poll_notification_permission() -> UnsafeMutablePointer<CChar>? {
    guard let value = YejianDocumentBridge.shared.pollNotificationPermission() else {
        return nil
    }
    return duplicate(value)
}

@_cdecl("yejian_ios_post_notification")
public func yejian_ios_post_notification(
    _ id: Int64,
    _ title: UnsafePointer<CChar>?,
    _ body: UnsafePointer<CChar>?,
    _ payload: UnsafePointer<CChar>?
) -> Int64 {
    guard let title, let body, let payload else { return -1 }
    YejianDocumentBridge.shared.postNotification(
        id: id,
        title: String(cString: title),
        body: String(cString: body),
        payload: String(cString: payload)
    )
    return 0
}

@_cdecl("yejian_ios_take_notification_open")
public func yejian_ios_take_notification_open() -> UnsafeMutablePointer<CChar>? {
    guard let payload = YejianDocumentBridge.shared.takeNotificationOpen() else {
        return nil
    }
    return duplicate(payload)
}

@_cdecl("yejian_ios_set_refresh_interval")
public func yejian_ios_set_refresh_interval(_ seconds: UInt64) {
    YejianDocumentBridge.shared.setRefreshInterval(seconds)
}

@_cdecl("yejian_ios_set_bg_refresh_handler")
public func yejian_ios_set_bg_refresh_handler(
    _ handler: (@convention(c) () -> UnsafeMutablePointer<CChar>?)?,
    _ free: (@convention(c) (UnsafeMutablePointer<CChar>?) -> Void)?
) {
    YejianDocumentBridge.shared.setBackgroundRefreshHandler(handler, free: free)
}
