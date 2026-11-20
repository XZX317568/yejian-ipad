package app.yejian.reader;

import android.app.NativeActivity;
import android.app.Dialog;
import android.content.ClipData;
import android.content.Intent;
import android.database.Cursor;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.util.Log;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.webkit.CookieManager;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.window.OnBackInvokedCallback;
import android.window.OnBackInvokedDispatcher;
import android.widget.Button;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.TextView;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicLong;

/**
 * Minimal NativeActivity shell around the Rust cdylib {@code libyejian.so}.
 *
 * <p>Rust owns the whole UI, lifecycle callbacks and the Android back key. This
 * class only adds the pieces that need the Java framework: the system document
 * picker, atomic copies into {@code filesDir/inbox}, the app-private paths and
 * the current window insets. Results are queued as JSON and drained by Rust
 * through {@link #pollImport()}; no Java callback has to run on a Rust thread.
 */
public class RustActivity extends NativeActivity {
    // Associate the Java callbacks with this class loader. NativeActivity
    // separately obtains the ANativeActivity_onCreate entry point.
    static {
        System.loadLibrary("yejian");
    }

    private static final String TAG = "YejianRust";
    private static final int REQUEST_OPEN_COMICS = 0x59454A31;
    private static final int REQUEST_CREATE_DOCUMENT = 0x59454A32;
    /** Archive picker: cbz/cbr/cbt/zip/rar/tar/gz. */
    static final int MODE_COMICS = 0;
    /** Backup picker: zip/json only. */
    static final int MODE_BACKUP = 1;

    /**
     * The picker only accepts archive-like MIME types. This is a filter, not a
     * storage permission: ACTION_OPEN_DOCUMENT grants access to the selected
     * URI only, so the app does not request READ_EXTERNAL_STORAGE or
     * MANAGE_EXTERNAL_STORAGE.
     */
    private static final String[] COMIC_MIME_TYPES = {
            "application/zip",
            "application/x-zip-compressed",
            "application/x-cbz",
            "application/vnd.comicbook+zip",
            "application/x-rar-compressed",
            "application/vnd.rar",
            "application/x-cbr",
            "application/vnd.comicbook-rar",
            "application/x-tar",
            "application/x-cbt",
            "application/gzip",
            "application/x-gzip",
            "application/octet-stream",
    };

    /**
     * The library backup produced by the UI is a zip or a json file. Narrowing
     * the picker keeps the "restore" flow from accepting comic archives.
     */
    private static final String[] BACKUP_MIME_TYPES = {
            "application/zip",
            "application/x-zip-compressed",
            "application/json",
            "text/json",
            "application/octet-stream",
    };

    private static volatile RustActivity instance;
    private static final AtomicLong requestIds = new AtomicLong(1);
    private static final AtomicLong activeRequestId = new AtomicLong(0);
    private static final AtomicInteger activeMode = new AtomicInteger(MODE_COMICS);
    private static final AtomicBoolean pickerInFlight = new AtomicBoolean(false);
    private static final AtomicBoolean importActive = new AtomicBoolean(false);
    private static volatile String pendingExportPath;
    private static final Object resultLock = new Object();
    private static final ArrayDeque<String> results = new ArrayDeque<>();
    /** WebView verification/login results; kept apart from import results. */
    private static final Object webResultLock = new Object();
    private static final ArrayDeque<String> webResults = new ArrayDeque<>();
    private static final AtomicBoolean webInFlight = new AtomicBoolean(false);
    private static volatile Dialog webDialog;
    private static final ExecutorService importer = Executors.newSingleThreadExecutor(runnable -> {
        Thread thread = new Thread(runnable, "yejian-import");
        thread.setDaemon(true);
        return thread;
    });

    private volatile int[] safeInsetsPx = new int[]{0, 0, 0, 0};
    /** Held as Object so the API 33 type is only touched behind a version check. */
    private Object backCallback;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Set before super.onCreate: the native thread can start during the
        // NativeActivity callback and may ask for the activity immediately.
        instance = this;
        super.onCreate(savedInstanceState);

        WindowInsets initial = getWindow().getDecorView().getRootWindowInsets();
        if (initial != null) {
            captureInsets(initial);
        }
        getWindow().getDecorView().setOnApplyWindowInsetsListener((view, insets) -> {
            captureInsets(insets);
            return insets;
        });
        getWindow().getDecorView().requestApplyInsets();

        registerBackHandling();

        // Cold start from "open with 页间".
        handleOpenIntent(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        if (intent != null) {
            setIntent(intent);
        }
        // Warm "open with" while the activity is alive (launchMode=singleTask).
        handleOpenIntent(intent);
    }

    @Override
    protected void onDestroy() {
        unregisterBackHandling();
        if (instance == this) {
            instance = null;
        }
        activeRequestId.set(0);
        pickerInFlight.set(false);
        pendingExportPath = null;
        super.onDestroy();
    }

    /**
     * Back belongs to Rust: the UI decides between "go back inside the app"
     * (reader → detail → library) and "finish" on the root page.
     *
     * <p>Two delivery paths exist:
     *
     * <ul>
     *   <li>Android 13+ predictive back (gesture, and with predictive back
     *       enabled for target 36 also the button) is dispatched through
     *       {@link OnBackInvokedDispatcher}, not as a key event, so the
     *       activity registers a callback that only records the request.</li>
     *   <li>Below API 33 the activity would finish itself; overriding
     *       {@link #onBackPressed()} keeps the same "record, never finish
     *       silently" contract.</li>
     * </ul>
     *
     * <p>3-button navigation on API 33+ still sends {@code KEYCODE_BACK} to the
     * native input queue first, and winit marks every key event handled, so in
     * that case Rust must also treat {@code egui::Key::BrowserBack} as back.
     */
    private void registerBackHandling() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return;
        }
        OnBackInvokedCallback callback = this::requestBack;
        backCallback = callback;
        getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT, callback);
    }

    private void unregisterBackHandling() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU
                || !(backCallback instanceof OnBackInvokedCallback)) {
            return;
        }
        getOnBackInvokedDispatcher().unregisterOnBackInvokedCallback(
                (OnBackInvokedCallback) backCallback);
        backCallback = null;
    }

    /** Records the back request for Rust; never finishes by itself. */
    private void requestBack() {
        nativeBackPressed();
    }

    @Override
    @SuppressWarnings("deprecation")
    public void onBackPressed() {
        // API < 33. Do not call super: the default silently finishes the app,
        // which would skip reader/detail navigation in Rust.
        requestBack();
    }

    /** Called by Rust when the UI is at the root page and wants to exit. */
    public static void finishApp() {
        RustActivity activity = instance;
        if (activity == null) {
            return;
        }
        activity.runOnUiThread(activity::finish);
    }

    /**
     * Opens the system picker. Returns a request id, or -1 when a picker is
     * already open or the activity is not ready.
     *
     * @param mode {@link #MODE_COMICS} or {@link #MODE_BACKUP}
     */
    public static long pickComics(int mode) {
        RustActivity activity = instance;
        if (activity == null) {
            return -1L;
        }
        if (mode != MODE_BACKUP) {
            mode = MODE_COMICS;
        }
        if (!pickerInFlight.compareAndSet(false, true)) {
            return -1L;
        }
        activeMode.set(mode);
        long requestId = requestIds.getAndIncrement();
        final int pickerMode = mode;
        activity.runOnUiThread(() -> {
            try {
                activity.launchPicker(requestId, pickerMode);
            } catch (Throwable error) {
                pickerInFlight.set(false);
                activeRequestId.set(0);
                enqueueResult(errorResult(requestId, pickerMode,
                        "无法打开系统文件选择器：" + describe(error)));
            }
        });
        return requestId;
    }

    /**
     * True while a picker is open or a copy is still running. Rust uses this to
     * poll at 250 ms only during an import instead of on every idle frame.
     */
    public static boolean isImporting() {
        return pickerInFlight.get() || importActive.get();
    }

    /** Returns the next completed import batch as JSON, or null when idle. */
    public static String pollImport() {
        synchronized (resultLock) {
            return results.pollFirst();
        }
    }

    /** Absolute {@code filesDir/inbox}, or an empty string before activity start. */
    public static String inboxDir() {
        RustActivity activity = instance;
        if (activity == null) {
            return "";
        }
        File directory = new File(activity.getFilesDir(), "inbox");
        if (!directory.exists() && !directory.mkdirs()) {
            Log.w(TAG, "Unable to create " + directory);
            return "";
        }
        return directory.getAbsolutePath();
    }

    /** Absolute {@code filesDir}. */
    public static String appDataDir() {
        RustActivity activity = instance;
        if (activity == null) {
            return "";
        }
        return activity.getFilesDir().getAbsolutePath();
    }

    /** Current insets as {@code "left,top,right,bottom"} physical pixels. */
    public static String safeInsetsPx() {
        RustActivity activity = instance;
        if (activity == null) {
            return "";
        }
        int[] values = activity.safeInsetsPx;
        return values[0] + "," + values[1] + "," + values[2] + "," + values[3];
    }

    /**
     * Hands an app-private file (a library backup) to the system share sheet so
     * the user can save it to Files/Drive or send it elsewhere.
     *
     * <p>The file is first copied into {@code cacheDir/share} and exposed
     * read-only through {@link FileShareProvider}; no storage permission and no
     * broad file access is involved.
     *
     * @return true when the sheet was requested, false when the file is missing
     *         or the activity is not ready
     */
    public static boolean shareFile(String path) {
        RustActivity activity = instance;
        if (activity == null || path == null || path.isEmpty()) {
            return false;
        }
        File source = new File(path);
        if (!source.isFile()) {
            return false;
        }
        activity.runOnUiThread(() -> {
            try {
                activity.startShare(source);
            } catch (Throwable error) {
                Log.w(TAG, "Unable to share " + source, error);
            }
        });
        return true;
    }

    /** Called by Rust once an import batch is queued. */
    private static native void nativeImportFinished();

    /** Called by the framework when the user presses/gestures back. */
    private static native void nativeBackPressed();

    /**
     * Opens `url` in the system browser through {@code ACTION_VIEW}.
     *
     * <p>Called by Rust for `link` settings items and external OAuth pages.
     * The Rust side already restricted the value to http/https; the flag
     * hardening and the try/catch keep a missing browser or a malformed URI
     * from ever crashing the native loop.
     *
     * @return true when the intent was handed to the system
     */
    public static boolean openExternalUrl(String url) {
        RustActivity activity = instance;
        if (activity == null || url == null || url.isEmpty()) {
            return false;
        }
        final String target = url;
        activity.runOnUiThread(() -> {
            try {
                Intent intent = new Intent(Intent.ACTION_VIEW, Uri.parse(target));
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                activity.startActivity(intent);
            } catch (Throwable error) {
                Log.w(TAG, "Unable to open " + target, error);
            }
        });
        return true;
    }

    /**
     * Opens the in-app WebView verification/login dialog. Returns false when
     * the activity is not ready or another web page is already open.
     *
     * <p>The dialog queues a JSON result (same contract as the iOS bridge)
     * that Rust drains through {@link #pollWeb()}; finishing collects the
     * cookies for the initial and the current URL so Cloudflare clearance
     * and login sessions survive.
     */
    public static boolean openWeb(String url, String userAgent) {
        RustActivity activity = instance;
        if (activity == null || url == null || url.isEmpty()) {
            return false;
        }
        if (!webInFlight.compareAndSet(false, true)) {
            return false;
        }
        final String agent = userAgent == null ? "" : userAgent;
        activity.runOnUiThread(() -> {
            try {
                activity.showWebDialog(url, agent);
            } catch (Throwable error) {
                webInFlight.set(false);
                Log.w(TAG, "Unable to open web dialog", error);
            }
        });
        return true;
    }

    /** Returns the next finished web page as JSON, or null while open. */
    public static String pollWeb() {
        synchronized (webResultLock) {
            return webResults.pollFirst();
        }
    }

    private static void enqueueWebResult(String payload) {
        synchronized (webResultLock) {
            webResults.addLast(payload);
        }
        webInFlight.set(false);
        nativeImportFinished();
    }

    /**
     * Full-screen WebView dialog: a top bar with the current site, 「取消」
     * and 「完成」 (each a ≥48dp touch target), then the page itself with
     * JavaScript, DOM storage and cookies enabled — the same surface Cloudflare
     * and site logins expect on Android WebView.
     */
    private void showWebDialog(String url, String userAgent) {
        final Dialog dialog = new Dialog(this,
                android.R.style.Theme_Black_NoTitleBar_Fullscreen);
        int density = (int) TypedValue.applyDimension(
                TypedValue.COMPLEX_UNIT_DIP, 1, getResources().getDisplayMetrics());
        LinearLayout column = new LinearLayout(this);
        column.setOrientation(LinearLayout.VERTICAL);

        LinearLayout bar = new LinearLayout(this);
        bar.setOrientation(LinearLayout.HORIZONTAL);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        int barHeight = 56 * density;
        bar.setMinimumHeight(barHeight);
        Button cancel = new Button(this);
        cancel.setText("取消");
        cancel.setMinHeight(48 * density);
        bar.addView(cancel, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        TextView title = new TextView(this);
        title.setGravity(Gravity.CENTER_VERTICAL);
        title.setSingleLine(true);
        title.setEllipsize(android.text.TextUtils.TruncateAt.MIDDLE);
        String host = Uri.parse(url).getHost();
        title.setText(host == null ? url : host);
        LinearLayout.LayoutParams titleParams = new LinearLayout.LayoutParams(
                0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f);
        titleParams.setMargins(12 * density, 0, 12 * density, 0);
        bar.addView(title, titleParams);
        Button done = new Button(this);
        done.setText("完成");
        done.setMinHeight(48 * density);
        bar.addView(done, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT));
        column.addView(bar, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT));

        final WebView webView = new WebView(this);
        WebSettings settings = webView.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        if (userAgent != null && !userAgent.isEmpty()) {
            settings.setUserAgentString(userAgent);
        }
        CookieManager cookies = CookieManager.getInstance();
        cookies.setAcceptCookie(true);
        cookies.setAcceptThirdPartyCookies(webView, true);
        webView.setWebViewClient(new WebViewClient() {
            @Override
            public boolean shouldOverrideUrlLoading(
                    WebView view, WebResourceRequest request) {
                Uri target = request.getUrl();
                String scheme = target.getScheme();
                // Regular http/https stays inside the WebView; custom schemes
                // (app callbacks, market links) are handed to the system.
                if ("http".equalsIgnoreCase(scheme) || "https".equalsIgnoreCase(scheme)) {
                    return false;
                }
                try {
                    startActivity(new Intent(Intent.ACTION_VIEW, target));
                } catch (Throwable ignored) {
                    // No handler for this scheme; keep the page open.
                }
                return true;
            }
        });
        column.addView(webView, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f));

        dialog.setContentView(column);
        android.view.Window window = dialog.getWindow();
        if (window != null) {
            window.setLayout(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT);
            // Respect the status/navigation bars so the action bar stays
            // reachable on notched devices.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                window.setDecorFitsSystemWindows(true);
            }
        }

        final String initialUrl = url;
        cancel.setOnClickListener(view -> dialog.dismiss());
        done.setOnClickListener(view -> {
            CookieManager manager = CookieManager.getInstance();
            manager.flush();
            // Collect cookies for both the requested and the navigated URL;
            // a login flow can end on a different origin than it started.
            java.util.LinkedHashMap<String, JSONObject> collected =
                    new java.util.LinkedHashMap<>();
            String currentUrl = webView.getUrl();
            for (String candidate : new String[]{initialUrl, currentUrl}) {
                if (candidate == null || candidate.isEmpty()) {
                    continue;
                }
                collectCookies(manager, candidate, collected);
            }
            try {
                JSONObject payload = new JSONObject();
                payload.put("url", currentUrl == null || currentUrl.isEmpty()
                        ? initialUrl : currentUrl);
                JSONArray list = new JSONArray();
                for (JSONObject cookie : collected.values()) {
                    list.put(cookie);
                }
                payload.put("cookies", list);
                enqueueWebResult(payload.toString());
            } catch (JSONException error) {
                enqueueWebResult("{\"url\":\"" + initialUrl + "\"}");
            }
            dialog.dismiss();
        });
        dialog.setOnDismissListener(d -> {
            webDialog = null;
            webView.destroy();
            // A dismissal without 「完成」 (cancel button or system back)
            // still reports the page so Rust can leave the pending state.
            if (webInFlight.compareAndSet(true, false)) {
                enqueueWebResult("{\"url\":\"" + initialUrl + "\"}");
            }
        });

        webView.loadUrl(url);
        webDialog = dialog;
        dialog.show();
    }

    /**
     * Adds every {@code name=value} pair the {@link CookieManager} reports for
     * `candidate` — and for its eTLD+1 origin when the URL is already a
     * subdomain — into `collected`, keyed so later origins only deduplicate.
     */
    private static void collectCookies(
            CookieManager manager, String candidate,
            java.util.LinkedHashMap<String, JSONObject> collected) {
        String header = manager.getCookie(candidate);
        Uri uri = Uri.parse(candidate);
        if ((header == null || header.isEmpty()) && uri.getHost() != null) {
            String host = uri.getHost();
            int dot = host.indexOf('.');
            if (dot > 0 && dot < host.length() - 1) {
                String parent = host.substring(dot + 1);
                header = manager.getCookie(uri.getScheme() + "://" + parent + "/");
            }
        }
        if (header == null) {
            return;
        }
        for (String pair : header.split(";")) {
            int equals = pair.indexOf('=');
            if (equals <= 0) {
                continue;
            }
            String name = pair.substring(0, equals).trim();
            String value = pair.substring(equals + 1).trim();
            if (name.isEmpty() || collected.containsKey(name)) {
                continue;
            }
            try {
                collected.put(name, object("name", name, "value", value));
            } catch (IllegalStateException ignored) {
                // object() only throws on an internal bug; skip the cookie.
            }
        }
    }


    /**
     * "Save as" for a library backup: opens {@code ACTION_CREATE_DOCUMENT} so
     * the user can pick any folder (Downloads, Drive, USB) and the app writes a
     * copy there. Complements {@link #shareFile(String)}, which opens the share
     * sheet; the share sheet does not guarantee a "save to Files" target.
     *
     * @return true when the save dialog was requested
     */
    public static boolean exportFile(String path) {
        RustActivity activity = instance;
        if (activity == null || path == null || path.isEmpty()) {
            return false;
        }
        File source = new File(path);
        if (!source.isFile()) {
            return false;
        }
        activity.runOnUiThread(() -> {
            try {
                pendingExportPath = source.getAbsolutePath();
                Intent intent = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType(mimeOf(source.getName()));
                intent.putExtra(Intent.EXTRA_TITLE, source.getName());
                activity.startActivityForResult(intent, REQUEST_CREATE_DOCUMENT);
            } catch (Throwable error) {
                pendingExportPath = null;
                Log.w(TAG, "Unable to export " + source, error);
            }
        });
        return true;
    }

    private void writeExport(File source, Uri target) {
        try (InputStream input = new FileInputStream(source);
             OutputStream output = getContentResolver().openOutputStream(target, "wt")) {
            if (output == null) {
                throw new IOException("系统没有返回可写入的输出流");
            }
            copy(input, output);
            output.flush();
        } catch (Exception error) {
            Log.w(TAG, "Backup export failed for " + target, error);
        }
    }

    private void startShare(File source) throws IOException {
        File shareDir = new File(getCacheDir(), "share");
        if (!shareDir.exists() && !shareDir.mkdirs()) {
            throw new IOException("无法创建导出目录 " + shareDir);
        }
        // One export at a time: drop copies from previous shares.
        File[] stale = shareDir.listFiles();
        if (stale != null) {
            for (File file : stale) {
                file.delete();
            }
        }
        File target = uniqueTarget(shareDir, sanitizeName(source.getName()));
        try (InputStream input = new FileInputStream(source);
             FileOutputStream output = new FileOutputStream(target)) {
            copy(input, output);
            output.flush();
            output.getFD().sync();
        } catch (IOException error) {
            target.delete();
            throw error;
        }

        Uri uri = FileShareProvider.uriFor(target);
        Intent send = new Intent(Intent.ACTION_SEND);
        send.setType(mimeOf(target.getName()));
        send.putExtra(Intent.EXTRA_STREAM, uri);
        send.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        startActivity(Intent.createChooser(send, "导出备份"));
    }

    private static String mimeOf(String name) {
        String lower = name == null ? "" : name.toLowerCase(java.util.Locale.ROOT);
        if (lower.endsWith(".json")) {
            return "application/json";
        }
        if (lower.endsWith(".zip") || lower.endsWith(".cbz")) {
            return "application/zip";
        }
        return "application/octet-stream";
    }

    private void launchPicker(long requestId, int mode) {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.putExtra(
                Intent.EXTRA_MIME_TYPES,
                mode == MODE_BACKUP ? BACKUP_MIME_TYPES : COMIC_MIME_TYPES);
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        activeRequestId.set(requestId);
        activeMode.set(mode);
        startActivityForResult(intent, REQUEST_OPEN_COMICS);
    }

    /**
     * "Open with 页间" (ACTION_VIEW) from a file manager, Downloads, a mail
     * attachment preview or another app.
     *
     * <p>Cold start delivers the intent to {@code onCreate}; because the
     * activity is {@code singleTask}, warm opens arrive in
     * {@link #onNewIntent(Intent)}. Both funnel into the same copy path as the
     * picker and produce a normal import batch, so Rust needs no extra API.
     *
     * <p>Kind is decided from the MIME type and the file name: JSON means a
     * library backup, everything else is treated as a comic archive. A backup
     * that is a plain {@code .zip} cannot be told apart from a cbz-style
     * archive by metadata, so it is reported as a comic; the user can still
     * restore it from the 设置 page.
     */
    private void handleOpenIntent(Intent intent) {
        if (intent == null || !Intent.ACTION_VIEW.equals(intent.getAction())) {
            return;
        }
        List<Uri> uris = collectUris(intent);
        if (uris.isEmpty()) {
            return;
        }
        Uri first = uris.get(0);
        int mode = modeFor(displayName(first), getContentResolver().getType(first));
        for (Uri uri : uris) {
            grantPersistable(uri);
        }
        long requestId = requestIds.getAndIncrement();
        importActive.set(true);
        importer.execute(() -> {
            try {
                importUris(requestId, mode, uris);
            } finally {
                importActive.set(false);
            }
        });
    }

    /** URIs carried by an intent: {@code getData()} plus any {@code ClipData}. */
    private static List<Uri> collectUris(Intent intent) {
        List<Uri> uris = new ArrayList<>();
        if (intent == null) {
            return uris;
        }
        ClipData clip = intent.getClipData();
        if (clip != null) {
            for (int index = 0; index < clip.getItemCount(); index++) {
                Uri uri = clip.getItemAt(index).getUri();
                if (uri != null && !uris.contains(uri)) {
                    uris.add(uri);
                }
            }
        }
        Uri single = intent.getData();
        if (single != null && !uris.contains(single)) {
            uris.add(single);
        }
        return uris;
    }

    /** JSON (by MIME or extension) is a backup; everything else is a comic. */
    private static int modeFor(String name, String mime) {
        String lowerName = name == null ? "" : name.toLowerCase(java.util.Locale.ROOT);
        String lowerMime = mime == null ? "" : mime.toLowerCase(java.util.Locale.ROOT);
        if (lowerMime.contains("json") || lowerName.endsWith(".json")) {
            return MODE_BACKUP;
        }
        return MODE_COMICS;
    }

    private void grantPersistable(Uri uri) {
        try {
            getContentResolver().takePersistableUriPermission(
                    uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } catch (SecurityException ignored) {
            // Providers do not have to offer persistable grants; the copy still
            // runs inside this activity's transient permission.
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode == REQUEST_CREATE_DOCUMENT) {
            String path = pendingExportPath;
            pendingExportPath = null;
            Uri target = resultCode == RESULT_OK && data != null ? data.getData() : null;
            if (path == null || target == null) {
                return;
            }
            File source = new File(path);
            importer.execute(() -> writeExport(source, target));
            return;
        }
        if (requestCode != REQUEST_OPEN_COMICS) {
            super.onActivityResult(requestCode, resultCode, data);
            return;
        }

        long requestId = activeRequestId.getAndSet(0);
        int mode = activeMode.getAndSet(MODE_COMICS);
        pickerInFlight.set(false);
        if (requestId == 0) {
            return;
        }
        if (resultCode != RESULT_OK || data == null) {
            enqueueResult(canceledResult(requestId, mode));
            return;
        }

        List<Uri> uris = collectUris(data);
        if (uris.isEmpty()) {
            enqueueResult(canceledResult(requestId, mode));
            return;
        }

        for (Uri uri : uris) {
            grantPersistable(uri);
        }
        importActive.set(true);
        importer.execute(() -> {
            try {
                importUris(requestId, mode, uris);
            } finally {
                importActive.set(false);
            }
        });
    }

    private void importUris(long requestId, int mode, List<Uri> uris) {
        File inbox = new File(getFilesDir(), "inbox");
        if (!inbox.exists() && !inbox.mkdirs()) {
            enqueueResult(errorResult(requestId, mode, "无法创建导入目录 " + inbox));
            return;
        }

        List<JSONObject> files = new ArrayList<>();
        List<JSONObject> failed = new ArrayList<>();
        for (Uri uri : uris) {
            String name = displayName(uri);
            try {
                File target = copyAtomically(uri, inbox, name);
                files.add(object("name", target.getName(), "path", target.getAbsolutePath()));
            } catch (Exception error) {
                Log.w(TAG, "Failed to import " + uri, error);
                failed.add(object("name", name, "reason", describe(error)));
            } finally {
                releasePersistable(uri);
            }
        }

        String status;
        if (failed.isEmpty()) {
            status = "ok";
        } else if (files.isEmpty()) {
            status = "error";
        } else {
            status = "partial";
        }
        enqueueResult(result(requestId, mode, status, files, failed, null));
    }

    private void releasePersistable(Uri uri) {
        try {
            getContentResolver().releasePersistableUriPermission(
                    uri, Intent.FLAG_GRANT_READ_URI_PERMISSION);
        } catch (Exception ignored) {
            // Not every provider grants persistable access; nothing to release.
        }
    }

    private File copyAtomically(Uri uri, File inbox, String displayName) throws IOException {
        String name = sanitizeName(displayName);
        File temporary = new File(inbox, ".import-" + UUID.randomUUID() + ".part");
        File target = uniqueTarget(inbox, name);
        try {
            InputStream input;
            try {
                input = getContentResolver().openInputStream(uri);
            } catch (SecurityException error) {
                throw new IOException("读取所选文件的权限已失效", error);
            }
            if (input == null) {
                throw new IOException("系统没有返回可读取的文件流");
            }
            try (InputStream source = input;
                 FileOutputStream output = new FileOutputStream(temporary)) {
                copy(source, output);
                output.flush();
                output.getFD().sync();
            }
        } catch (IOException error) {
            temporary.delete();
            throw error;
        }

        if (!temporary.renameTo(target)) {
            temporary.delete();
            throw new IOException("无法完成原子导入：" + target.getName());
        }
        return target;
    }

    private static void copy(InputStream input, OutputStream output) throws IOException {
        byte[] buffer = new byte[64 * 1024];
        int read;
        while ((read = input.read(buffer)) != -1) {
            output.write(buffer, 0, read);
        }
    }

    private String displayName(Uri uri) {
        String name = null;
        try (Cursor cursor = getContentResolver().query(
                uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int index = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (index >= 0) {
                    name = cursor.getString(index);
                }
            }
        } catch (Exception ignored) {
            // Fall back to the URI's last path segment.
        }
        if (name == null || name.trim().isEmpty()) {
            String segment = uri.getLastPathSegment();
            name = segment == null ? "import-" + System.currentTimeMillis() : segment;
        }
        return name;
    }

    private static String sanitizeName(String raw) {
        String name = raw == null ? "" : raw
                .replace('/', '_')
                .replace('\\', '_')
                .replace('\u0000', '_')
                .trim();
        if (name.isEmpty() || name.equals(".") || name.equals("..")) {
            name = "import-" + System.currentTimeMillis();
        }
        if (name.length() > 180) {
            int dot = name.lastIndexOf('.');
            String extension = dot > 0 ? name.substring(dot) : "";
            name = name.substring(0, Math.max(1, 180 - extension.length())) + extension;
        }
        return name;
    }

    private static File uniqueTarget(File directory, String name) {
        int dot = name.lastIndexOf('.');
        String base = dot > 0 ? name.substring(0, dot) : name;
        String extension = dot > 0 ? name.substring(dot) : "";
        File candidate = new File(directory, name);
        int index = 2;
        while (candidate.exists()) {
            candidate = new File(directory, base + " (" + index + ")" + extension);
            index++;
        }
        return candidate;
    }

    private void captureInsets(WindowInsets insets) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Insets values = insets.getInsets(
                    WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            safeInsetsPx = new int[]{values.left, values.top, values.right, values.bottom};
        } else {
            safeInsetsPx = new int[]{
                    insets.getSystemWindowInsetLeft(),
                    insets.getSystemWindowInsetTop(),
                    insets.getSystemWindowInsetRight(),
                    insets.getSystemWindowInsetBottom(),
            };
        }
    }

    private static JSONObject object(Object... pairs) {
        JSONObject value = new JSONObject();
        try {
            for (int index = 0; index + 1 < pairs.length; index += 2) {
                value.put((String) pairs[index], pairs[index + 1]);
            }
        } catch (JSONException error) {
            throw new IllegalStateException(error);
        }
        return value;
    }

    private static JSONArray array(List<JSONObject> values) {
        JSONArray result = new JSONArray();
        for (JSONObject value : values) {
            result.put(value);
        }
        return result;
    }

    private static String kindOf(int mode) {
        return mode == MODE_BACKUP ? "backup" : "comics";
    }

    private static String result(long requestId, int mode, String status,
                                 List<JSONObject> files, List<JSONObject> failed,
                                 String message) {
        try {
            JSONObject payload = new JSONObject();
            payload.put("requestId", requestId);
            payload.put("kind", kindOf(mode));
            payload.put("status", status);
            payload.put("files", array(files));
            payload.put("failed", array(failed));
            if (message != null) {
                payload.put("message", message);
            }
            return payload.toString();
        } catch (JSONException error) {
            throw new IllegalStateException(error);
        }
    }

    private static String canceledResult(long requestId, int mode) {
        return result(requestId, mode, "canceled", new ArrayList<>(), new ArrayList<>(), null);
    }

    private static String errorResult(long requestId, int mode, String message) {
        return result(requestId, mode, "error", new ArrayList<>(), new ArrayList<>(), message);
    }

    private static void enqueueResult(String payload) {
        synchronized (resultLock) {
            results.addLast(payload);
        }
        nativeImportFinished();
    }

    private static String describe(Throwable error) {
        String message = error.getMessage();
        return message == null || message.isEmpty()
                ? error.getClass().getSimpleName()
                : message;
    }
}
