package cc.thro.throcc

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

@Composable
fun Onboarding(model: AppModel) {
    var name by rememberSaveable { mutableStateOf("") }
    var picture by rememberSaveable { mutableStateOf<ByteArray?>(null) }
    val notices = @Composable { SnackbarHost(model.snackbars) }
    Scaffold(snackbarHost = notices, contentWindowInsets = WindowInsets.safeDrawing) { padding ->
        Box(Modifier.padding(padding).fillMaxSize().verticalScroll(rememberScrollState()), Alignment.Center) {
            Column(Modifier.widthIn(max = 480.dp).padding(24.dp), Arrangement.spacedBy(24.dp)) {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text("Welcome to throcc", style = MaterialTheme.typography.displaySmallEmphasized)
                    Text(
                        "Choose the name people see you as.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodyLarge,
                    )
                }
                Panel {
                    PictureField(model, picture, name) { picture = it }
                    NameField(name) { name = it }
                }
                MainButton("Continue", validName(name) != null) { model.saveProfile(name, picture) }
            }
        }
    }
}
