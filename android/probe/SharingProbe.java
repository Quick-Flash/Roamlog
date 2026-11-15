package dev.roamlog.android;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.net.Uri;
import android.os.Bundle;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import java.io.InputStream;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import org.json.JSONArray;
import org.json.JSONObject;

/** Disposable foreground-only networking harness. No GPS or recorder integration. */
public final class SharingProbe extends Activity {
    static { System.loadLibrary("roamlog"); }
    private static native boolean prepare();
    private static native void stop();
    private static native void vote(boolean accepted);
    private static native String decode(int width, int height, byte[] gray);
    private native void run(String mode, String target, String name);
    private native void command(String mode, String target);

    private TextView peer;
    private TextView address;
    private TextView status;
    private EditText target;
    private Button identity;
    private Button listen;
    private Button connect;
    private Button cancel;
    private boolean destroyed;
    private boolean pairingActive;
    private FriendsPanel friends;
    private LocationPanel locations;
    private boolean locationSession;
    private final ExecutorService sharingControls = Executors.newSingleThreadExecutor();

    @SuppressWarnings("deprecation")
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        LinearLayout content = new LinearLayout(this);
        content.setOrientation(LinearLayout.VERTICAL);
        int padding = (int) (16 * getResources().getDisplayMetrics().density);
        content.setPadding(padding, padding, padding, padding);
        TextView title = new TextView(this);
        title.setText("Roamlog sharing prototype\nConnectivity, pairing, and explicit test locations. No GPS access.");
        content.addView(title);
        peer = new TextView(this);
        peer.setTextIsSelectable(true);
        content.addView(peer);
        address = new TextView(this);
        address.setTextIsSelectable(true);
        content.addView(address);
        Button copy = new Button(this);
        copy.setText("Copy address JSON");
        copy.setOnClickListener(view -> {
            if (address.getText().length() != 0) {
                ClipboardManager clipboard = (ClipboardManager) getSystemService(CLIPBOARD_SERVICE);
                clipboard.setPrimaryClip(ClipData.newPlainText("Probe address", address.getText()));
            }
        });
        content.addView(copy);
        target = new EditText(this);
        target.setHint("Invitation link, or peer ID/address JSON for networking tests");
        target.setMinLines(3);
        content.addView(target);
        identity = button(content, "Show identity", "identity");
        listen = button(content, "Listen", "listen");
        connect = button(content, "Connect and watch", "connect");
        cancel = new Button(this);
        cancel.setText("Stop");
        cancel.setEnabled(false);
        cancel.setOnClickListener(view -> stop());
        content.addView(cancel);
        status = new TextView(this);
        status.setText("Stopped");
        content.addView(status);
        friends = new FriendsPanel(this, content, target, this::begin, SharingProbe::vote, this::scan);
        locations = new LocationPanel(this, content, target, this::begin, this::control);
        ScrollView scroll = new ScrollView(this);
        scroll.setOnApplyWindowInsetsListener((view, insets) -> {
            content.setPadding(padding + insets.getSystemWindowInsetLeft(),
                padding + insets.getSystemWindowInsetTop(),
                padding + insets.getSystemWindowInsetRight(),
                padding + insets.getSystemWindowInsetBottom());
            return insets;
        });
        scroll.addView(content);
        setContentView(scroll);
        invitation(getIntent());
    }

    private Button button(LinearLayout content, String label, String mode) {
        Button button = new Button(this);
        button.setText(label);
        button.setOnClickListener(view -> begin(mode));
        content.addView(button);
        return button;
    }

    private void busy(boolean running) {
        identity.setEnabled(!running);
        listen.setEnabled(!running);
        connect.setEnabled(!running);
        cancel.setEnabled(running);
        friends.busy(running);
        locations.busy(running);
    }

    private void begin(String mode) {
        begin(mode, target.getText().toString().trim());
    }

    private void begin(String mode, String input) {
        if (locationSession) {
            control(mode, input);
            return;
        }
        if ((mode.equals("listen") || mode.equals("connect") || mode.equals("pair") || mode.equals("location_connect")) && input.isEmpty()) {
            status.setText("Enter peer ID or address JSON first");
            return;
        }
        if (!prepare()) {
            status.setText("Previous probe still stopping; retry shortly");
            return;
        }
        busy(true);
        pairingActive = mode.equals("invite") || mode.equals("pair");
        locationSession = mode.equals("location_listen") || mode.equals("location_connect");
        String displayName = friends.name();
        address.setText("");
        status.setText("Loading encrypted identity");
        new Thread(() -> {
            try { run(mode, input, displayName); }
            catch (Throwable error) { progress("Native probe failed"); }
            finally { runOnUiThread(() -> {
                pairingActive = false; locationSession = false;
                if (!destroyed) { friends.locationSession(false); locations.active(false); busy(false); }
            }); }
        }, "sharing-probe").start();
    }

    /** JNI callback runs on probe worker. Public because native lookup uses this name. */
    public void progress(String message) {
        runOnUiThread(() -> {
            if (destroyed) return;
            try {
                if (message.startsWith("peer: ")) peer.setText(message.substring(6));
                else if (message.startsWith("address: ")) address.setText(message.substring(9));
                else if (message.startsWith("invite:")) {
                    friends.invitation(new JSONObject(message.substring(7)));
                    status.setText("Invitation open for five minutes. Share QR or copy link.");
                } else if (message.startsWith("confirm:")) {
                    friends.pending(new JSONObject(message.substring(8)));
                    status.setText("Both users must confirm matching verification code");
                } else if (message.startsWith("friends:")) friends.friends(new JSONArray(message.substring(8)));
                else if (message.startsWith("mode:")) {
                    locations.mode(new JSONObject("{\"mode\":" + message.substring(5) + "}").getString("mode"));
                    locations.active(locationSession);
                    friends.locationSession(locationSession);
                } else if (message.startsWith("locations:")) locations.locations(new JSONArray(message.substring(10)));
                else status.setText(message);
            } catch (Exception error) { status.setText("Invalid native response"); stop(); }
        });
    }

    private void control(String mode, String input) {
        sharingControls.execute(() -> {
            try { command(mode, input); }
            catch (Throwable error) { progress("Sharing control failed"); }
        });
    }

    private void invitation(Intent intent) {
        Uri uri = intent == null ? null : intent.getData();
        if (uri != null && "roamlog".equals(uri.getScheme()) && "pair".equals(uri.getHost())) {
            target.setText(uri.toString());
            status.setText("Review invitation, then tap Pair from invitation link");
        }
    }

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        stop();
        friends.busy(true);
        invitation(intent);
        if (!pairingActive) friends.busy(false);
    }

    @SuppressWarnings("deprecation")
    private void scan() {
        Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT).setType("image/*")
            .addCategory(Intent.CATEGORY_OPENABLE);
        startActivityForResult(pick, 17);
    }

    @SuppressWarnings("deprecation")
    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request != 17 || result != RESULT_OK || data == null || data.getData() == null) return;
        Uri uri = data.getData();
        busy(true);
        cancel.setEnabled(false);
        new Thread(() -> {
            try {
                BitmapFactory.Options options = new BitmapFactory.Options();
                options.inJustDecodeBounds = true;
                try (InputStream input = getContentResolver().openInputStream(uri)) {
                    BitmapFactory.decodeStream(input, null, options);
                }
                if (options.outWidth <= 0 || options.outHeight <= 0) throw new Exception("Invalid image");
                options.inSampleSize = 1;
                while (options.outWidth / options.inSampleSize > 1024 || options.outHeight / options.inSampleSize > 1024)
                    options.inSampleSize *= 2;
                options.inJustDecodeBounds = false;
                Bitmap bitmap;
                try (InputStream input = getContentResolver().openInputStream(uri)) {
                    bitmap = BitmapFactory.decodeStream(input, null, options);
                }
                if (bitmap == null || bitmap.getWidth() > 2048 || bitmap.getHeight() > 2048)
                    throw new Exception("Invalid image");
                int width = bitmap.getWidth();
                int height = bitmap.getHeight();
                int[] colors = new int[width * height];
                bitmap.getPixels(colors, 0, width, 0, 0, width, height);
                bitmap.recycle();
                byte[] gray = new byte[colors.length];
                for (int i = 0; i < colors.length; i++) {
                    int color = colors[i];
                    gray[i] = (byte) (((color >> 16 & 255) * 299 + (color >> 8 & 255) * 587 + (color & 255) * 114) / 1000);
                }
                String link = decode(width, height, gray);
                runOnUiThread(() -> { if (!destroyed) { target.setText(link); status.setText("QR decoded. Review, then tap Pair."); } });
            } catch (Throwable error) { progress("QR image invalid; choose clear photo or screenshot of one invitation"); }
            finally { runOnUiThread(() -> { if (!destroyed) busy(false); }); }
        }, "sharing-qr").start();
    }

    @Override protected void onStop() {
        // Bounded pairing stays alive briefly so invitation links can be sent in another app.
        if (!pairingActive) stop();
        super.onStop();
    }

    @Override protected void onDestroy() {
        destroyed = true;
        stop();
        sharingControls.shutdownNow();
        super.onDestroy();
    }
}
