package cc.thro.throcc

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
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
    Scaffold { padding ->
        Column(
            Modifier
                .padding(padding)
                .consumeWindowInsets(padding)
                .imePadding()
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
            Alignment.CenterHorizontally,
        ) {
            Text("Welcome to throcc", style = MaterialTheme.typography.headlineMedium)
            Text("Choose the name people see you as.", color = MaterialTheme.colorScheme.onSurfaceVariant)
            Column(Modifier.widthIn(max = 480.dp), Arrangement.spacedBy(16.dp)) {
                PictureField(model, picture, name) { picture = it }
                NameField(name) { name = it }
            }
            Button({ model.saveProfile(name, picture) }, enabled = validName(name) != null) { Text("Continue") }
        }
    }
}
