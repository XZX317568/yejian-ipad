package app.yejian.reader;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.database.MatrixCursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileNotFoundException;
import java.io.IOException;
import java.util.List;

/**
 * Read-only content provider for files copied into {@code cacheDir/share}.
 *
 * <p>Deliberately dependency-free (no AndroidX {@code FileProvider}) so the
 * Gradle build stays offline-friendly. It only exposes the single
 * {@code share} directory and never resolves outside it, so the app does not
 * need to grant access to its whole storage.
 */
public class FileShareProvider extends ContentProvider {
    static final String AUTHORITY = "app.yejian.reader.rust.files";
    private static final String SHARE_DIRECTORY = "share";

    /** Builds the URI for a file that lives directly inside {@code share}. */
    static Uri uriFor(File file) {
        return new Uri.Builder()
                .scheme("content")
                .authority(AUTHORITY)
                .appendPath(file.getName())
                .build();
    }

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        if (mode != null && (mode.contains("w") || mode.contains("a"))) {
            throw new FileNotFoundException("FileShareProvider is read-only");
        }
        File file = resolve(uri);
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY);
    }

    @Override
    public String getType(Uri uri) {
        String name = uri.getLastPathSegment();
        String lower = name == null ? "" : name.toLowerCase(java.util.Locale.ROOT);
        if (lower.endsWith(".json")) {
            return "application/json";
        }
        if (lower.endsWith(".zip") || lower.endsWith(".cbz")) {
            return "application/zip";
        }
        return "application/octet-stream";
    }

    @Override
    public Cursor query(Uri uri, String[] projection, String selection,
                        String[] selectionArgs, String sortOrder) {
        File file;
        try {
            file = resolve(uri);
        } catch (FileNotFoundException error) {
            return null;
        }
        String[] columns = projection == null
                ? new String[]{OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE}
                : projection;
        MatrixCursor cursor = new MatrixCursor(columns, 1);
        Object[] row = new Object[columns.length];
        for (int index = 0; index < columns.length; index++) {
            if (OpenableColumns.DISPLAY_NAME.equals(columns[index])) {
                row[index] = file.getName();
            } else if (OpenableColumns.SIZE.equals(columns[index])) {
                row[index] = file.length();
            }
        }
        cursor.addRow(row);
        return cursor;
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException("FileShareProvider is read-only");
    }

    @Override
    public int delete(Uri uri, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("FileShareProvider is read-only");
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) {
        throw new UnsupportedOperationException("FileShareProvider is read-only");
    }

    private File resolve(Uri uri) throws FileNotFoundException {
        if (!AUTHORITY.equals(uri.getAuthority())) {
            throw new FileNotFoundException("unknown authority: " + uri);
        }
        List<String> segments = uri.getPathSegments();
        if (segments.size() != 1) {
            throw new FileNotFoundException("expected one path segment: " + uri);
        }
        File directory = new File(getContext().getCacheDir(), SHARE_DIRECTORY);
        File file = new File(directory, segments.get(0));
        try {
            String root = directory.getCanonicalPath() + File.separator;
            if (!file.getCanonicalPath().startsWith(root)) {
                throw new FileNotFoundException("path escapes the share directory: " + uri);
            }
        } catch (IOException error) {
            throw new FileNotFoundException(error.getMessage());
        }
        if (!file.isFile()) {
            throw new FileNotFoundException(uri.toString());
        }
        return file;
    }
}
