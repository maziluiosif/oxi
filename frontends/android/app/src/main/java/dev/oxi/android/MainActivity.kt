package dev.oxi.android

import android.net.Uri
import android.provider.Settings
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.IntentCompat
import android.os.Bundle
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.oxi.android.ui.ChatScreen
import dev.oxi.android.ui.ChatsScreen
import dev.oxi.android.ui.GitSheet
import dev.oxi.android.ui.Oxi
import dev.oxi.android.ui.OxiTheme
import dev.oxi.android.ui.AddServerScreen
import dev.oxi.android.ui.WorkspacesScreen

class MainActivity : ComponentActivity() {
    private val vm: OxiViewModel by viewModels()

    // Registered here, not in a composable: the camera app can push oxi out of memory, and the
    // photo must still find its way back when the activity is rebuilt.
    private val camera = registerForActivityResult(ActivityResultContracts.TakePicture()) { ok -> vm.photoTaken(ok) }
    private val notifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) (application as OxiApp).watch.permissionGranted()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        // A rebuilt activity gets the intent it was first started with again.
        if (savedInstanceState == null) handle(intent)
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.STARTED) {
                Updater.events.collect { event ->
                    when (event) {
                        is Updater.InstallEvent.Confirm -> runCatching { startActivity(event.intent) }
                            .onFailure { vm.updateFailed("Couldn't open the installer: ${it.message}") }
                        is Updater.InstallEvent.Failed -> vm.updateFailed(event.message)
                    }
                }
            }
        }
        setContent {
            OxiTheme {
                val state by vm.ui.collectAsStateWithLifecycle()
                val context = LocalContext.current
                LaunchedEffect(state.toast) {
                    state.toast?.let {
                        Toast.makeText(context, it, Toast.LENGTH_SHORT).show()
                        vm.toastShown()
                    }
                }
                val screen = when {
                    state.servers.isEmpty() || state.adding -> 0
                    state.chat != null -> 3
                    state.root != null -> 2
                    else -> 1
                }
                BackHandler(enabled = screen >= 2) {
                    if (screen == 3) vm.closeChat() else vm.openWorkspace(null)
                }
                Box(Modifier.fillMaxSize().background(Oxi.bg)) {
                    AnimatedContent(
                        targetState = screen,
                        transitionSpec = { fadeIn(tween(160)) togetherWith fadeOut(tween(120)) },
                        label = "screen",
                    ) { s ->
                        when (s) {
                            0 -> AddServerScreen(vm, state)
                            1 -> WorkspacesScreen(vm, state)
                            2 -> ChatsScreen(vm, state)
                            else -> ChatScreen(vm, state)
                        }
                    }
                }
                state.git?.let { GitSheet(vm, it) }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    override fun onResume() {
        super.onResume()
        vm.onResume()
        // Back from Android's install confirmation without installing: offer the update again.
        // (When the update goes ahead, this process is replaced before the delay is up.)
        lifecycleScope.launch {
            delay(4000)
            vm.updateDismissed()
        }
    }

    /** Install the downloaded update, first sending the user to allow installs if needed. */
    fun installUpdate() {
        if (!vm.installUpdate()) {
            vm.toast("Allow oxi to install updates, then tap Update again")
            startActivity(
                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName")),
            )
        }
    }

    override fun onPause() {
        super.onPause()
        vm.persistDrafts()
    }

    fun takePhoto() {
        runCatching { camera.launch(vm.newPhoto()) }
            .onFailure { vm.photoTaken(false); vm.toast("No camera app to take the photo with") }
    }

    /** Ask once to show notifications: they tell when a reply is ready or needs an answer. */
    fun askForNotifications() {
        if (Build.VERSION.SDK_INT < 33) return
        val prefs = ServerStore.prefs(this)
        if (prefs.getBoolean("asked_notifications", false)) return
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return
        prefs.edit().putBoolean("asked_notifications", true).apply()
        notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
    }

    private fun handle(intent: Intent?) {
        when (intent?.action) {
            TurnWatch.ACTION_OPEN_CHAT -> vm.openChatAt(
                intent.getStringExtra(TurnWatch.EXTRA_ROOT).orEmpty(),
                intent.getStringExtra(TurnWatch.EXTRA_CHAT) ?: return,
                intent.getStringExtra(TurnWatch.EXTRA_TITLE).orEmpty(),
            )
            Intent.ACTION_SEND -> vm.share(
                sharedText(intent),
                listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)),
            )
            Intent.ACTION_SEND_MULTIPLE -> vm.share(
                sharedText(intent),
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty(),
            )
            else -> intent?.data?.let { vm.pairFromUri(it) }
        }
    }

    /** A shared link usually comes with the page's title as the subject. */
    private fun sharedText(intent: Intent): String? {
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.trim().orEmpty()
        val subject = intent.getStringExtra(Intent.EXTRA_SUBJECT)?.trim().orEmpty()
        return listOf(subject.takeIf { it.isNotEmpty() && it !in text }, text.takeIf { it.isNotEmpty() })
            .filterNotNull().joinToString("\n").takeIf { it.isNotEmpty() }
    }
}
