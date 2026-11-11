package cc.thro.throcc

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import cc.thro.throcc.core.CoreState
import cc.thro.throcc.core.fingerprintShort

private const val SOURCE = "https://github.com/nylyk/throcc"

@Composable
fun Settings(model: AppModel) {
    val pages = listOf(
        "Profile" to Screen.Profile,
        "Appearance" to Screen.Appearance,
        "Verified People" to Screen.Verified,
        "Blocked People" to Screen.Blocked,
        "About" to Screen.About,
    )
    Scaffold(topBar = { TopBar("Settings", model::back) }) { padding ->
        Column(Modifier.padding(padding).verticalScroll(rememberScrollState())) {
            pages.forEach { (title, screen) ->
                ListItem({ Text(title) }, Modifier.clickable { model.open(screen) })
            }
        }
    }
}

@Composable
fun Profile(model: AppModel) {
    val profile = model.profile ?: return
    var name by rememberSaveable { mutableStateOf(profile.name) }
    var picture by rememberSaveable { mutableStateOf(profile.picture) }
    val changed = validName(name) != profile.name || !picture.contentEquals(profile.picture)
    Scaffold(topBar = { TopBar("Profile", model::back) }) { padding ->
        Form(padding) {
            PictureField(model, picture, name) { picture = it }
            NameField(name) { name = it }
            Button(
                { if (model.saveProfile(name, picture)) model.back() },
                enabled = changed && validName(name) != null,
            ) { Text("Save") }
        }
    }
}

@Composable
fun Appearance(model: AppModel) {
    val accents = remember { model.core.accents() }
    val custom = model.accent.takeIf { chosen -> accents.none { it.name == chosen.name } }
    var hex by rememberSaveable { mutableStateOf(custom?.rgb?.toString(16)?.padStart(6, '0')?.uppercase().orEmpty()) }
    Scaffold(topBar = { TopBar("Appearance", model::back) }) { padding ->
        Column(
            Modifier.padding(padding).consumeWindowInsets(padding).imePadding().verticalScroll(rememberScrollState()),
        ) {
            SectionTitle("Accent")
            accents.forEach { accent ->
                val selected = accent.name == model.accent.name
                ListItem(
                    { Text(accent.name) },
                    Modifier.selectable(selected, role = Role.RadioButton) { model.pickAccent(accent.name) },
                    leadingContent = { Swatch(accent.rgb) },
                    trailingContent = { RadioButton(selected, null) },
                )
            }
            OutlinedTextField(
                hex,
                { typed ->
                    hex = typed.filter { it in "0123456789abcdefABCDEF" }.take(6).uppercase()
                    hex.takeIf { it.length == 6 }?.toUIntOrNull(16)?.let(model::pickCustomAccent)
                },
                Modifier.fillMaxWidth().padding(16.dp),
                label = { Text("Custom color") },
                leadingIcon = custom?.let { { Swatch(it.rgb) } },
                prefix = { Text("#") },
                keyboardOptions = KeyboardOptions(KeyboardCapitalization.Characters, autoCorrectEnabled = false),
                singleLine = true,
            )
        }
    }
}

@Composable
private fun Swatch(rgb: UInt) {
    Box(Modifier.size(24.dp).background(accentColor(rgb), CircleShape))
}

@Composable
fun VerifiedPeople(model: AppModel, state: CoreState) {
    var removed by remember { mutableIntStateOf(0) }
    val people = remember(state.people.map { it.verified }, removed) { model.core.verified() }
    Scaffold(topBar = { TopBar("Verified People", model::back) }) { padding ->
        if (people.isEmpty()) {
            Hint("Nobody yet. Compare safety numbers with someone to verify them.", Modifier.padding(padding))
        }
        LazyColumn(contentPadding = padding) {
            items(people) { person ->
                ListItem(
                    { Text(person.name) },
                    supportingContent = {
                        Column {
                            if (person.servers.isNotEmpty()) Text(person.servers.joinToString(", "))
                            Text(fingerprintShort(person.key), fontFamily = FontFamily.Monospace)
                        }
                    },
                    trailingContent = {
                        IconButton({ if (model.tell { model.core.unverify(person.key) }) removed++ }) {
                            Icon(Icons.Default.Close, "Mark as Not Verified")
                        }
                    },
                )
            }
        }
    }
}

@Composable
fun BlockedPeople(model: AppModel, state: CoreState) {
    var removed by remember { mutableIntStateOf(0) }
    val people = remember(state.people.map { it.blocked }, removed) { model.core.blocked() }
    Scaffold(topBar = { TopBar("Blocked People", model::back) }) { padding ->
        if (people.isEmpty()) Hint("Nobody. Tap someone in a room to block them.", Modifier.padding(padding))
        LazyColumn(contentPadding = padding) {
            items(people) { person ->
                ListItem(
                    { Text(person.name) },
                    supportingContent = {
                        Column {
                            person.server?.let { Text(it) }
                            Text(fingerprintShort(person.key), fontFamily = FontFamily.Monospace)
                        }
                    },
                    leadingContent = { Avatar(remember(person) { person.picture?.toImage() }, person.name, 40.dp) },
                    trailingContent = {
                        TextButton({ if (model.tell { model.core.unblock(person.key) }) removed++ }) { Text("Unblock") }
                    },
                )
            }
        }
    }
}

@Composable
fun About(model: AppModel) {
    val context = LocalContext.current
    val links = LocalUriHandler.current
    val version = remember { context.packageManager.getPackageInfo(context.packageName, 0).versionName.orEmpty() }
    Scaffold(topBar = { TopBar("About", model::back) }) { padding ->
        Column(Modifier.padding(padding)) {
            ListItem({ Text("Version") }, trailingContent = { Text(version) })
            ListItem(
                { Text("Source") },
                Modifier.clickable { links.openUri(SOURCE) },
                supportingContent = { Text(SOURCE) },
            )
        }
    }
}
