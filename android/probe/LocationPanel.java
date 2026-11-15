package dev.roamlog.android;

import android.app.Activity;
import android.text.InputType;
import android.widget.Button;
import android.widget.CheckBox;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;
import java.util.ArrayList;
import java.util.List;
import java.util.function.BiConsumer;
import org.json.JSONArray;
import org.json.JSONObject;

/** Explicit sample controls; this screen never opens a GPS provider. */
final class LocationPanel {
    private final Activity activity;
    private final List<Button> controls = new ArrayList<>();
    private final List<Button> starts = new ArrayList<>();
    private final TextView mode;
    private final TextView received;
    private final EditText latitude;
    private final EditText longitude;
    private final CheckBox recording;
    private boolean active;

    LocationPanel(Activity activity, LinearLayout content, EditText target,
            BiConsumer<String, String> launch, BiConsumer<String, String> command) {
        this.activity = activity;
        TextView title = new TextView(activity);
        title.setText("Location protocol test\nExplicit test coordinates only. No GPS collection. Pair friends first.");
        content.addView(title);
        starts.add(button(content, "Listen for paired friends", () -> launch.accept("location_listen", "")));
        starts.add(button(content, "Connect to friend's address JSON", () -> launch.accept("location_connect", target.getText().toString().trim())));
        mode = new TextView(activity);
        mode.setText("Global sharing: Off");
        content.addView(mode);
        controls.add(button(content, "Pause all sharing", () -> command.accept("mode_off", "")));
        controls.add(button(content, "Share only during test trip", () -> command.accept("mode_trips", "")));
        controls.add(button(content, "Enable sharing during test", () -> command.accept("mode_always", "")));
        latitude = input(content, "Test latitude (-90 to 90)");
        longitude = input(content, "Test longitude (-180 to 180)");
        recording = new CheckBox(activity);
        recording.setText("Test trip recording (simulation)");
        recording.setOnCheckedChangeListener((button, checked) -> { if (active) command.accept("recording", Boolean.toString(checked)); });
        content.addView(recording);
        controls.add(button(content, "Publish latest test fix", () -> {
            try {
                JSONObject sample = new JSONObject().put("latitude", Double.parseDouble(latitude.getText().toString()))
                    .put("longitude", Double.parseDouble(longitude.getText().toString()))
                    .put("recording", recording.isChecked());
                command.accept("sample", sample.toString());
            } catch (Exception error) { mode.setText("Enter valid test coordinates"); }
        }));
        controls.add(button(content, "Mark GPS unavailable (test)", () -> command.accept("clear_fix", "")));
        received = new TextView(activity);
        received.setTextIsSelectable(true);
        received.setText("No received locations");
        content.addView(received);
        active(false);
    }

    private Button button(LinearLayout content, String label, Runnable action) {
        Button button = new Button(activity);
        button.setText(label);
        button.setOnClickListener(view -> action.run());
        content.addView(button);
        return button;
    }

    private EditText input(LinearLayout content, String hint) {
        EditText input = new EditText(activity);
        input.setHint(hint);
        input.setInputType(InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_FLAG_DECIMAL | InputType.TYPE_NUMBER_FLAG_SIGNED);
        content.addView(input);
        return input;
    }

    void active(boolean active) {
        this.active = active;
        for (Button button : controls) button.setEnabled(active);
        latitude.setEnabled(active);
        longitude.setEnabled(active);
        recording.setEnabled(active);
        if (!active) { recording.setChecked(false); mode.setText("Sharing session stopped"); }
    }

    void busy(boolean busy) { for (Button button : starts) button.setEnabled(!busy); }

    void mode(String value) { mode.setText("Global sharing: " + value + ". Friend permission also required."); }

    void locations(JSONArray peers) throws Exception {
        StringBuilder text = new StringBuilder();
        for (int i = 0; i < peers.length(); i++) {
            JSONObject peer = peers.getJSONObject(i);
            text.append(peer.getString("peer_id")).append('\n');
            text.append(peer.getBoolean("connected") ? "Connected" : "Offline");
            text.append(peer.getBoolean("sharing_active") ? " · sharing active" : " · sharing inactive").append('\n');
            if (!peer.isNull("latest")) {
                JSONObject location = peer.getJSONObject("latest");
                text.append(location.getInt("latitude_e7") / 1e7).append(", ")
                    .append(location.getInt("longitude_e7") / 1e7).append('\n');
                text.append("Age ").append(peer.getLong("age_ms") / 1000).append(" seconds · accuracy ")
                    .append(location.getLong("accuracy_cm") / 100.0).append(" m")
                    .append(peer.getBoolean("stale") ? " · stale" : " · fresh").append('\n');
            } else text.append("Position unavailable\n");
        }
        received.setText(text.length() == 0 ? "No received locations" : text.toString());
    }
}
