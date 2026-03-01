import { component$ } from "@builder.io/qwik";
import { QwikCityProvider, RouterOutlet } from "@builder.io/qwik-city";
import { RouterHead } from "./components/router-head/router-head";

import "./global.css";

export default component$(() => {
  return (
    <QwikCityProvider>
      <head>
        <meta charset="utf-8" />
        <RouterHead />
      </head>
      <body lang="en" class="bg-gray-950 text-gray-100" style="background-color:#09090b;color:#f3f4f6">
        <RouterOutlet />
      </body>
    </QwikCityProvider>
  );
});
