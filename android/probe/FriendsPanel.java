package dev.roamlog.android;

import android.app.Activity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.widget.Button;
import android.widget.EditText;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.function.BiConsumer;
import java.util.function.Consumer;
import org.json.JSONArray;
import org.json.JSONObject;

/** Phase 2 controls; Rust owns pairing, persistence, and authorization. */
final class FriendsPanel {
    private final Activity activity;
    private final BiConsumer<String, String> launch;
    private final List<Button> actions = new ArrayList<>();
    private final List<Button> rowActions = new ArrayList<>();
    private final EditText name;
    private final TextView confirmation;
    private final Button accept;
    private final Button reject;
    private final ImageView qr;
    private final LinearLayout rows;
    private final Button copy;
    private String invitation = "";
    private boolean busy;
    private boolean locationSession;

    FriendsPanel(Activity activity, LinearLayout content, EditText target,
            BiConsumer<String, String> launch, Consumer<Boolean> vote, Runnable scan) {
        this.activity = activity;
        this.launch = launch;
        TextView title = new TextView(activity);
        title.setText("Friends\nPairing starts with location access Off. Global sharing must also be enabled.");
        content.addView(title);
        name = new EditText(activity);
        name.setHint("Your name");
        name.setSingleLine(true);
        name.setText(activity.getPreferences(0).getString("sharing_name", "Roamlog"));
        content.addView(name);
        action(content, "Add friend: show invitation QR", () -> launch.accept("invite", ""));
        action(content, "Pair from invitation link", () -> launch.accept("pair", target.getText().toString().trim()));
        action(content, "Scan QR image", scan);
        action(content, "Refresh friends", () -> launch.accept("friends", ""));
        copy = new Button(activity);
        copy.setText("Copy invitation link");
        copy.setEnabled(false);
        copy.setOnClickListener(view -> {
            ClipboardManager clipboard = (ClipboardManager) activity.getSystemService(Activity.CLIPBOARD_SERVICE);
            clipboard.setPrimaryClip(ClipData.newPlainText("Roamlog invitation", invitation));
        });
        content.addView(copy);
        qr = new ImageView(activity);
        qr.setContentDescription("Friend invitation QR code");
        qr.setAdjustViewBounds(true);
        content.addView(qr);
        confirmation = new TextView(activity);
        confirmation.setTextIsSelectable(true);
        content.addView(confirmation);
        accept = new Button(activity);
        accept.setText("Codes match: confirm pairing");
        accept.setEnabled(false);
        content.addView(accept);
        reject = new Button(activity);
        reject.setText("Reject pairing");
        reject.setEnabled(false);
        accept.setOnClickListener(view -> { accept.setEnabled(false); reject.setEnabled(false); vote.accept(true); });
        reject.setOnClickListener(view -> { accept.setEnabled(false); reject.setEnabled(false); vote.accept(false); });
        content.addView(reject);
        rows = new LinearLayout(activity);
        rows.setOrientation(LinearLayout.VERTICAL);
        content.addView(rows);
    }

    private void action(LinearLayout content, String text, Runnable click) {
        Button button = new Button(activity);
        button.setText(text);
        button.setOnClickListener(view -> click.run());
        content.addView(button);
        actions.add(button);
    }

    String name() {
        String value = name.getText().toString().trim();
        activity.getPreferences(0).edit().putString("sharing_name", value).apply();
        return value;
    }

    void busy(boolean running) {
        busy = running;
        name.setEnabled(!running);
        for (Button button : actions) button.setEnabled(!running);
        for (Button button : rowActions) button.setEnabled(!running || locationSession);
        accept.setEnabled(false);
        reject.setEnabled(false);
        if (!running) {
            confirmation.setText("");
            invitation = "";
            qr.setImageDrawable(null);
            copy.setEnabled(false);
        }
    }

    void locationSession(boolean active) {
        locationSession = active;
        for (Button button : rowActions) button.setEnabled(!busy || active);
    }

    void invitation(JSONObject data) throws Exception {
        invitation = data.getString("link");
        JSONArray modules = data.getJSONArray("qr");
        int size = modules.length();
        int scale = 6;
        int pixels = (size + 8) * scale;
        Bitmap bitmap = Bitmap.createBitmap(pixels, pixels, Bitmap.Config.ARGB_8888);
        bitmap.eraseColor(Color.WHITE);
        for (int y = 0; y < size; y++) {
            String row = modules.getString(y);
            for (int x = 0; x < size; x++) if (row.charAt(x) == '1') {
                for (int dy = 0; dy < scale; dy++) for (int dx = 0; dx < scale; dx++)
                    bitmap.setPixel((x + 4) * scale + dx, (y + 4) * scale + dy, Color.BLACK);
            }
        }
        qr.setImageBitmap(bitmap);
        copy.setEnabled(true);
    }

    void pending(JSONObject data) throws Exception {
        qr.setImageDrawable(null);
        copy.setEnabled(false);
        confirmation.setText("Verify matching code on both phones:\n" + data.getString("code")
            + "\nFriend: " + data.getString("display_name") + "\nPeer: " + data.getString("peer_id"));
        accept.setEnabled(true);
        reject.setEnabled(true);
    }

    void friends(JSONArray friends) throws Exception {
        rows.removeAllViews();
        rowActions.clear();
        for (int i = 0; i < friends.length(); i++) {
            JSONObject friend = friends.getJSONObject(i);
            String id = friend.getString("peer_id");
            boolean blocked = friend.getBoolean("blocked");
            TextView label = new TextView(activity);
            label.setText(friend.getString("display_name") + "\n" + id + "\nLocation access: "
                + (blocked ? "Blocked" : friend.getString("permission")));
            label.setTextIsSelectable(true);
            rows.addView(label);
            if (!blocked) {
                rowAction("Off", "permission_off", id);
                rowAction("Trip only", "permission_trips", id);
                rowAction("Always", "permission_always", id);
                rowAction("Block", "block", id);
            }
            rowAction("Remove", "remove", id);
        }
        if (friends.length() == 0) {
            TextView empty = new TextView(activity);
            empty.setText("No paired friends");
            rows.addView(empty);
        }
    }

    private void rowAction(String text, String mode, String id) {
        Button button = new Button(activity);
        button.setText(text);
        button.setEnabled(!busy || locationSession);
        button.setOnClickListener(view -> launch.accept(mode, id));
        rows.addView(button);
        rowActions.add(button);
    }
}
